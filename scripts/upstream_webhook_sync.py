#!/usr/bin/env python3
"""
Webhook-driven upstream sync orchestration.

Implements:
1) GitHub webhook receiver for upstream push notifications
2) Queue-backed worker that runs scripts/sync-upstream.sh
3) Queue backends:
   - sqlite (default, no extra dependencies)
   - sqs (optional boto3)
   - kafka (optional kafka-python)

This keeps unattended syncs reproducible while still allowing strict risk gates
to block sensitive upstream changes for human/agent implementation work.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import hmac
import json
import logging
import os
import pathlib
import re
import shutil
import sqlite3
import subprocess
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


LOG = logging.getLogger("upstream-webhook-sync")

DEFAULT_UPSTREAM_REPO = "NousResearch/hermes-agent"
DEFAULT_UPSTREAM_REF = "refs/heads/main"


def utc_now_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


@dataclass
class SyncEvent:
    delivery_id: str
    event_name: str
    repository: str
    ref: str
    before_sha: str
    after_sha: str
    compare_url: str
    pushed_at: str
    payload: dict[str, Any]

    def to_json(self) -> str:
        return json.dumps(
            {
                "delivery_id": self.delivery_id,
                "event_name": self.event_name,
                "repository": self.repository,
                "ref": self.ref,
                "before_sha": self.before_sha,
                "after_sha": self.after_sha,
                "compare_url": self.compare_url,
                "pushed_at": self.pushed_at,
                "payload": self.payload,
            },
            sort_keys=True,
        )


@dataclass
class QueueItem:
    id: int
    attempts: int
    event: SyncEvent


class SqliteQueue:
    def __init__(self, db_path: str) -> None:
        self.db_path = db_path
        parent = pathlib.Path(db_path).parent
        parent.mkdir(parents=True, exist_ok=True)
        self._init_schema()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, timeout=30)
        conn.row_factory = sqlite3.Row
        return conn

    def _init_schema(self) -> None:
        with self._connect() as conn:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS queue_events (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  delivery_id TEXT NOT NULL UNIQUE,
                  event_name TEXT NOT NULL,
                  repository TEXT NOT NULL,
                  ref TEXT NOT NULL,
                  before_sha TEXT NOT NULL,
                  after_sha TEXT NOT NULL,
                  compare_url TEXT NOT NULL,
                  pushed_at TEXT NOT NULL,
                  payload_json TEXT NOT NULL,
                  status TEXT NOT NULL DEFAULT 'pending',
                  attempts INTEGER NOT NULL DEFAULT 0,
                  last_error TEXT,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_queue_events_status_created ON queue_events(status, id)"
            )
            conn.commit()

    def enqueue(self, event: SyncEvent) -> bool:
        now = utc_now_iso()
        try:
            with self._connect() as conn:
                conn.execute(
                    """
                    INSERT INTO queue_events(
                      delivery_id, event_name, repository, ref, before_sha, after_sha,
                      compare_url, pushed_at, payload_json, status, attempts,
                      created_at, updated_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)
                    """,
                    (
                        event.delivery_id,
                        event.event_name,
                        event.repository,
                        event.ref,
                        event.before_sha,
                        event.after_sha,
                        event.compare_url,
                        event.pushed_at,
                        json.dumps(event.payload),
                        now,
                        now,
                    ),
                )
                conn.commit()
                return True
        except sqlite3.IntegrityError:
            # Duplicate GitHub delivery id -> idempotent accept.
            return False

    def claim_next(self) -> QueueItem | None:
        with self._connect() as conn:
            conn.execute("BEGIN IMMEDIATE")
            row = conn.execute(
                """
                SELECT * FROM queue_events
                WHERE status = 'pending'
                ORDER BY id ASC
                LIMIT 1
                """
            ).fetchone()
            if row is None:
                conn.commit()
                return None

            new_attempts = int(row["attempts"]) + 1
            conn.execute(
                """
                UPDATE queue_events
                SET status='processing', attempts=?, updated_at=?
                WHERE id=?
                """,
                (new_attempts, utc_now_iso(), row["id"]),
            )
            conn.commit()

            event = SyncEvent(
                delivery_id=row["delivery_id"],
                event_name=row["event_name"],
                repository=row["repository"],
                ref=row["ref"],
                before_sha=row["before_sha"],
                after_sha=row["after_sha"],
                compare_url=row["compare_url"],
                pushed_at=row["pushed_at"],
                payload=json.loads(row["payload_json"]),
            )
            return QueueItem(id=row["id"], attempts=new_attempts, event=event)

    def mark_done(self, item_id: int, status: str = "done", note: str | None = None) -> None:
        with self._connect() as conn:
            conn.execute(
                """
                UPDATE queue_events
                SET status=?, last_error=?, updated_at=?
                WHERE id=?
                """,
                (status, note, utc_now_iso(), item_id),
            )
            conn.commit()

    def mark_retry_or_dead(
        self, item_id: int, attempts: int, max_attempts: int, error: str
    ) -> None:
        target_status = "dead" if attempts >= max_attempts else "pending"
        with self._connect() as conn:
            conn.execute(
                """
                UPDATE queue_events
                SET status=?, last_error=?, updated_at=?
                WHERE id=?
                """,
                (target_status, error, utc_now_iso(), item_id),
            )
            conn.commit()


class SqsPublisher:
    def __init__(self, queue_url: str, region: str | None) -> None:
        try:
            import boto3  # type: ignore
        except Exception as exc:  # pragma: no cover
            raise RuntimeError(
                "SQS backend requires boto3. Install with: pip install boto3"
            ) from exc
        self.client = boto3.client("sqs", region_name=region)
        self.queue_url = queue_url

    def enqueue(self, event: SyncEvent) -> bool:
        self.client.send_message(
            QueueUrl=self.queue_url,
            MessageBody=event.to_json(),
            MessageGroupId="upstream-sync",
            MessageDeduplicationId=event.delivery_id,
        )
        return True


class SqsConsumer:
    def __init__(self, queue_url: str, region: str | None) -> None:
        try:
            import boto3  # type: ignore
        except Exception as exc:  # pragma: no cover
            raise RuntimeError(
                "SQS backend requires boto3. Install with: pip install boto3"
            ) from exc
        self.client = boto3.client("sqs", region_name=region)
        self.queue_url = queue_url

    def receive(self, wait_seconds: int = 20) -> tuple[SyncEvent, str] | None:
        resp = self.client.receive_message(
            QueueUrl=self.queue_url,
            MaxNumberOfMessages=1,
            WaitTimeSeconds=max(0, min(wait_seconds, 20)),
            VisibilityTimeout=120,
        )
        msgs = resp.get("Messages") or []
        if not msgs:
            return None

        msg = msgs[0]
        body = json.loads(msg["Body"])
        event = SyncEvent(
            delivery_id=body["delivery_id"],
            event_name=body["event_name"],
            repository=body["repository"],
            ref=body["ref"],
            before_sha=body["before_sha"],
            after_sha=body["after_sha"],
            compare_url=body["compare_url"],
            pushed_at=body["pushed_at"],
            payload=body.get("payload", {}),
        )
        return event, msg["ReceiptHandle"]

    def ack(self, receipt_handle: str) -> None:
        self.client.delete_message(QueueUrl=self.queue_url, ReceiptHandle=receipt_handle)


class KafkaPublisher:
    def __init__(self, bootstrap: str, topic: str) -> None:
        try:
            from kafka import KafkaProducer  # type: ignore
        except Exception as exc:  # pragma: no cover
            raise RuntimeError(
                "Kafka backend requires kafka-python. Install with: pip install kafka-python"
            ) from exc
        self.topic = topic
        self.producer = KafkaProducer(
            bootstrap_servers=[bootstrap],
            value_serializer=lambda v: json.dumps(v).encode("utf-8"),
            key_serializer=lambda k: k.encode("utf-8"),
        )

    def enqueue(self, event: SyncEvent) -> bool:
        payload = json.loads(event.to_json())
        self.producer.send(self.topic, key=event.delivery_id, value=payload).get(timeout=10)
        return True


class KafkaConsumerWrapper:
    def __init__(self, bootstrap: str, topic: str, group_id: str) -> None:
        try:
            from kafka import KafkaConsumer  # type: ignore
        except Exception as exc:  # pragma: no cover
            raise RuntimeError(
                "Kafka backend requires kafka-python. Install with: pip install kafka-python"
            ) from exc
        self.consumer = KafkaConsumer(
            topic,
            bootstrap_servers=[bootstrap],
            group_id=group_id,
            enable_auto_commit=False,
            auto_offset_reset="latest",
            value_deserializer=lambda v: json.loads(v.decode("utf-8")),
        )

    def poll(self, timeout_ms: int = 1000) -> tuple[SyncEvent, Any] | None:
        batch = self.consumer.poll(timeout_ms=timeout_ms, max_records=1)
        for _, records in batch.items():
            if not records:
                continue
            rec = records[0]
            body = rec.value
            event = SyncEvent(
                delivery_id=body["delivery_id"],
                event_name=body["event_name"],
                repository=body["repository"],
                ref=body["ref"],
                before_sha=body["before_sha"],
                after_sha=body["after_sha"],
                compare_url=body["compare_url"],
                pushed_at=body["pushed_at"],
                payload=body.get("payload", {}),
            )
            return event, rec
        return None

    def ack(self) -> None:
        self.consumer.commit()


def verify_github_signature(body: bytes, secret: str, signature_header: str | None) -> bool:
    if not secret:
        return True
    if not signature_header:
        return False
    expected = "sha256=" + hmac.new(
        secret.encode("utf-8"), body, hashlib.sha256
    ).hexdigest()
    return hmac.compare_digest(expected, signature_header)


def make_event_from_payload(delivery_id: str, event_name: str, payload: dict[str, Any]) -> SyncEvent:
    repo = (
        payload.get("repository", {})
        .get("full_name", "")
        .strip()
    )
    ref = str(payload.get("ref", "")).strip()
    before_sha = str(payload.get("before", "")).strip()
    after_sha = str(payload.get("after", "")).strip()
    compare_url = str(payload.get("compare", "")).strip()
    return SyncEvent(
        delivery_id=delivery_id,
        event_name=event_name,
        repository=repo,
        ref=ref,
        before_sha=before_sha,
        after_sha=after_sha,
        compare_url=compare_url,
        pushed_at=utc_now_iso(),
        payload=payload,
    )


def parse_report_status(report_path: str) -> str:
    if not report_path or not os.path.exists(report_path):
        return "unknown"
    status = "unknown"
    risk_status = ""
    with open(report_path, "r", encoding="utf-8") as fh:
        for line in fh:
            s = line.strip()
            if s.startswith("status:"):
                status = s.split(":", 1)[1].strip()
            if s.startswith("risk_gate_status:"):
                risk_status = s.split(":", 1)[1].strip()
    if risk_status == "blocked":
        return "risk_blocked"
    if status == "conflict":
        return "conflict"
    return status


def run_git(repo_root: str, args: list[str], check: bool = True) -> tuple[int, str, str]:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def git_ref_exists(repo_root: str, ref: str) -> bool:
    rc, _, _ = run_git(repo_root, ["rev-parse", "--verify", ref], check=False)
    return rc == 0


def git_show_file(repo_root: str, ref: str, rel_path: str) -> str | None:
    rc, out, _ = run_git(repo_root, ["show", f"{ref}:{rel_path}"], check=False)
    if rc != 0:
        return None
    return out


def rust_fn_block(source: str, fn_name: str) -> str | None:
    fn_re = re.compile(rf"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+{re.escape(fn_name)}\b")
    m = fn_re.search(source)
    if not m:
        return None
    start = m.start()
    next_fn_re = re.compile(r"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+[A-Za-z0-9_]+")
    n = next_fn_re.search(source, m.end())
    end = n.start() if n else len(source)
    return source[start:end]


def extract_actions_from_rust_fn(source: str, fn_name: str) -> list[str]:
    block = rust_fn_block(source, fn_name)
    if not block:
        return []
    actions: set[str] = set()
    for m in re.finditer(r'unwrap_or\("([A-Za-z0-9_-]+)"\)', block):
        actions.add(m.group(1))
    for raw in block.splitlines():
        line = raw.strip()
        if "=>" not in line:
            continue
        if not (
            line.startswith("Some(")
            or line.startswith("None")
            or line.startswith('"')
        ):
            continue
        for m in re.finditer(r'"([A-Za-z0-9_-]+)"', line):
            actions.add(m.group(1))
    return sorted(actions)


def camel_to_kebab(name: str) -> str:
    out: list[str] = []
    for idx, ch in enumerate(name):
        if ch.isupper() and idx > 0:
            out.append("-")
        out.append(ch.lower())
    return "".join(out)


def extract_top_level_cli_commands(cli_source: str) -> list[str]:
    names = []
    for m in re.finditer(r"(?m)^\s{4}([A-Z][A-Za-z0-9_]*)\s*(?:\{|,)", cli_source):
        variant = m.group(1)
        names.append(camel_to_kebab(variant))
    return sorted(set(names))


def collect_cli_surface(repo_root: str, ref: str) -> dict[str, Any] | None:
    cli_rs = git_show_file(repo_root, ref, "crates/hermes-cli/src/cli.rs")
    main_rs = git_show_file(repo_root, ref, "crates/hermes-cli/src/main.rs")
    commands_rs = git_show_file(repo_root, ref, "crates/hermes-cli/src/commands.rs")
    if not cli_rs or not main_rs or not commands_rs:
        return None

    fn_map: dict[str, tuple[str, str]] = {
        "tools": ("main", "run_tools"),
        "gateway": ("main", "run_gateway"),
        "auth": ("main", "run_auth"),
        "cron": ("main", "run_cron"),
        "webhook": ("main", "run_webhook"),
        "profile": ("main", "run_profile"),
        "memory": ("commands", "handle_cli_memory"),
        "mcp": ("commands", "handle_cli_mcp"),
        "skills": ("commands", "handle_cli_skills"),
    }
    source_lookup = {"main": main_rs, "commands": commands_rs}
    actions: dict[str, list[str]] = {}
    for command_name, (src_key, fn_name) in fn_map.items():
        actions[command_name] = extract_actions_from_rust_fn(source_lookup[src_key], fn_name)
    return {
        "ref": ref,
        "top_level": extract_top_level_cli_commands(cli_rs),
        "actions": actions,
    }


def compute_cli_surface_drift(
    local_surface: dict[str, Any], upstream_surface: dict[str, Any]
) -> dict[str, Any]:
    local_top = set(local_surface.get("top_level", []))
    upstream_top = set(upstream_surface.get("top_level", []))
    top_missing = sorted(upstream_top - local_top)
    top_extra = sorted(local_top - upstream_top)

    all_commands = sorted(
        set(local_surface.get("actions", {}).keys())
        | set(upstream_surface.get("actions", {}).keys())
    )
    per_command: dict[str, dict[str, list[str]]] = {}
    missing_total = 0
    for command_name in all_commands:
        local_actions = set(local_surface.get("actions", {}).get(command_name, []))
        upstream_actions = set(upstream_surface.get("actions", {}).get(command_name, []))
        missing = sorted(upstream_actions - local_actions)
        extra = sorted(local_actions - upstream_actions)
        if missing or extra:
            per_command[command_name] = {
                "missing_in_local": missing,
                "extra_in_local": extra,
            }
            missing_total += len(missing)

    has_drift = bool(top_missing or missing_total > 0)
    return {
        "has_drift": has_drift,
        "top_level": {
            "missing_in_local": top_missing,
            "extra_in_local": top_extra,
        },
        "actions": per_command,
        "missing_action_count": missing_total,
    }


def load_seen_drift_fingerprints(path: str) -> dict[str, Any]:
    if not os.path.exists(path):
        return {"fingerprints": {}}
    try:
        with open(path, "r", encoding="utf-8") as fh:
            raw = json.load(fh)
        if isinstance(raw, dict) and isinstance(raw.get("fingerprints"), dict):
            return raw
    except Exception:
        pass
    return {"fingerprints": {}}


def save_seen_drift_fingerprints(path: str, payload: dict[str, Any]) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, indent=2, sort_keys=True)


def gh_issue_comment(repo_root: str, issue: int, body: str) -> bool:
    if shutil.which("gh") is None:
        return False
    proc = subprocess.run(
        ["gh", "issue", "comment", str(issue), "--body", body],
        cwd=repo_root,
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        LOG.warning("gh issue comment failed: %s", proc.stderr.strip())
        return False
    return True


def gh_issue_create(
    repo_root: str, title: str, body: str, labels: list[str]
) -> str | None:
    if shutil.which("gh") is None:
        return None
    cmd = ["gh", "issue", "create", "--title", title, "--body", body]
    for label in labels:
        if label.strip():
            cmd.extend(["--label", label.strip()])
    proc = subprocess.run(
        cmd,
        cwd=repo_root,
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        LOG.warning("gh issue create failed: %s", proc.stderr.strip())
        return None
    return proc.stdout.strip().splitlines()[-1].strip() if proc.stdout.strip() else None


def run_python_script(
    repo_root: str, argv: list[str], timeout_sec: int = 1800
) -> tuple[int, str]:
    proc = subprocess.run(
        ["python3", *argv],
        cwd=repo_root,
        text=True,
        capture_output=True,
        timeout=max(60, timeout_sec),
    )
    out = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
    return proc.returncode, out


def maybe_check_global_parity_drift(
    *,
    repo_root: str,
    report_dir: str,
    event: SyncEvent,
    parent_issue: int,
    open_parity_issue: bool,
    parity_labels: list[str],
    enabled: bool,
    max_queue_commits: int,
) -> dict[str, Any]:
    if not enabled:
        return {"enabled": False, "checked": False}

    scripts = [
        ["scripts/generate-parity-matrix.py"],
        ["scripts/generate-workstream-status.py"],
        ["scripts/generate-test-intent-mapping.py"],
        ["scripts/generate-adapter-matrix.py"],
        ["scripts/validate-intentional-divergence.py", "--check", "--allow-warnings"],
        [
            "scripts/generate-upstream-patch-queue.py",
            "--max-commits",
            str(max(0, max_queue_commits)),
        ],
        ["scripts/generate-global-parity-proof.py", "--check-ci"],
    ]

    runs: list[dict[str, Any]] = []
    final_rc = 0
    for argv in scripts:
        rc, output = run_python_script(repo_root, argv)
        runs.append({"argv": argv, "rc": rc, "output_tail": output[-4000:]})
        if rc != 0:
            final_rc = rc
            break

    proof_path = os.path.join(repo_root, "docs/parity/global-parity-proof.json")
    proof = {}
    if os.path.exists(proof_path):
        try:
            with open(proof_path, "r", encoding="utf-8") as fh:
                proof = json.load(fh)
        except Exception:
            proof = {}
    ci_gate = proof.get("ci_gate", {}) if isinstance(proof, dict) else {}
    has_drift = final_rc != 0 or not bool(ci_gate.get("pass"))

    artifact = {
        "generated_at": utc_now_iso(),
        "delivery_id": event.delivery_id,
        "repository": event.repository,
        "upstream_after_sha": event.after_sha,
        "has_drift": has_drift,
        "script_runs": runs,
        "proof_path": proof_path if os.path.exists(proof_path) else "",
        "proof_ci_gate": ci_gate,
    }
    os.makedirs(report_dir, exist_ok=True)
    artifact_path = os.path.join(report_dir, f"global-parity-drift-{event.delivery_id}.json")
    with open(artifact_path, "w", encoding="utf-8") as fh:
        json.dump(artifact, fh, indent=2, sort_keys=True)

    result = {
        "enabled": True,
        "checked": True,
        "artifact_path": artifact_path,
        "has_drift": has_drift,
        "commented_parent_issue": False,
        "created_issue_url": None,
    }
    if not has_drift:
        return result

    fingerprint_payload = {
        "ci_checks": ci_gate.get("checks", []),
        "upstream_after": event.after_sha,
        "failing_script": runs[-1]["argv"] if runs else [],
    }
    fingerprint = hashlib.sha256(
        json.dumps(fingerprint_payload, sort_keys=True).encode("utf-8")
    ).hexdigest()
    seen_path = os.path.join(report_dir, "global-parity-drift-seen.json")
    seen = load_seen_drift_fingerprints(seen_path)

    summary_lines = [
        "Global parity drift detected.",
        f"- delivery_id: `{event.delivery_id}`",
        f"- upstream_after: `{event.after_sha}`",
        f"- proof_path: `{proof_path}`",
        f"- artifact: `{artifact_path}`",
        f"- ci_gate_pass: `{bool(ci_gate.get('pass'))}`",
    ]
    comment_body = "\n".join(summary_lines)
    if parent_issue > 0:
        result["commented_parent_issue"] = gh_issue_comment(repo_root, parent_issue, comment_body)

    existing = seen.get("fingerprints", {}).get(fingerprint)
    if open_parity_issue and not existing:
        title = f"[Parity Drift] Global parity gate drift ({event.after_sha[:12]})"
        issue_body = "\n".join(
            [
                "Automated global parity audit detected CI-gate drift.",
                "",
                f"- delivery_id: `{event.delivery_id}`",
                f"- upstream_after: `{event.after_sha}`",
                f"- proof_path: `{proof_path}`",
                f"- artifact: `{artifact_path}`",
                "",
                "Please process drift items and keep queue dispositions current.",
            ]
        )
        created = gh_issue_create(repo_root, title, issue_body, parity_labels)
        if created:
            result["created_issue_url"] = created
            seen.setdefault("fingerprints", {})[fingerprint] = {
                "created_at": utc_now_iso(),
                "issue": created,
                "delivery_id": event.delivery_id,
            }
            save_seen_drift_fingerprints(seen_path, seen)
    return result


def maybe_check_cli_surface_drift(
    *,
    repo_root: str,
    report_dir: str,
    event: SyncEvent,
    upstream_ref: str,
    parent_issue: int,
    open_parity_issue: bool,
    parity_labels: list[str],
    enabled: bool,
) -> dict[str, Any]:
    if not enabled:
        return {"enabled": False, "checked": False}
    local_ref = "HEAD"
    local_surface = collect_cli_surface(repo_root, local_ref)
    if local_surface is None:
        return {"enabled": True, "checked": False, "reason": "local_cli_surface_missing"}
    if not git_ref_exists(repo_root, upstream_ref):
        return {
            "enabled": True,
            "checked": False,
            "reason": f"upstream_ref_missing:{upstream_ref}",
        }
    upstream_surface = collect_cli_surface(repo_root, upstream_ref)
    if upstream_surface is None:
        return {
            "enabled": True,
            "checked": False,
            "reason": f"upstream_cli_surface_missing:{upstream_ref}",
        }

    drift = compute_cli_surface_drift(local_surface, upstream_surface)
    generated_at = utc_now_iso()
    artifact = {
        "generated_at": generated_at,
        "delivery_id": event.delivery_id,
        "repository": event.repository,
        "upstream_ref": upstream_ref,
        "local_ref": local_ref,
        "upstream_after_sha": event.after_sha,
        "drift": drift,
        "local_surface": local_surface,
        "upstream_surface": upstream_surface,
    }
    os.makedirs(report_dir, exist_ok=True)
    artifact_path = os.path.join(report_dir, f"cli-surface-drift-{event.delivery_id}.json")
    with open(artifact_path, "w", encoding="utf-8") as fh:
        json.dump(artifact, fh, indent=2, sort_keys=True)

    result = {
        "enabled": True,
        "checked": True,
        "artifact_path": artifact_path,
        "has_drift": bool(drift.get("has_drift")),
        "commented_parent_issue": False,
        "created_issue_url": None,
    }
    if not drift.get("has_drift"):
        return result

    fingerprint_payload = {
        "top_level_missing": drift.get("top_level", {}).get("missing_in_local", []),
        "action_missing": {
            cmd: details.get("missing_in_local", [])
            for cmd, details in sorted(drift.get("actions", {}).items())
            if details.get("missing_in_local")
        },
    }
    fingerprint = hashlib.sha256(
        json.dumps(fingerprint_payload, sort_keys=True).encode("utf-8")
    ).hexdigest()
    seen_path = os.path.join(report_dir, "cli-surface-drift-seen.json")
    seen = load_seen_drift_fingerprints(seen_path)

    missing_top = drift.get("top_level", {}).get("missing_in_local", [])
    missing_action_count = int(drift.get("missing_action_count", 0))
    summary_lines = [
        "CLI surface drift detected.",
        f"- delivery_id: `{event.delivery_id}`",
        f"- upstream_ref: `{upstream_ref}`",
        f"- upstream_after: `{event.after_sha}`",
        f"- missing_top_level: `{len(missing_top)}`",
        f"- missing_actions: `{missing_action_count}`",
        f"- artifact: `{artifact_path}`",
    ]
    if missing_top:
        summary_lines.append(f"- missing_top_level_names: {', '.join(missing_top[:20])}")
    comment_body = "\n".join(summary_lines)
    if parent_issue > 0:
        result["commented_parent_issue"] = gh_issue_comment(repo_root, parent_issue, comment_body)

    existing = seen.get("fingerprints", {}).get(fingerprint)
    if open_parity_issue and not existing:
        title = f"[Parity Drift] CLI surface drift from {upstream_ref} ({event.after_sha[:12]})"
        issue_body = "\n".join(
            [
                "Automated parity drift detector flagged missing upstream command/action surface.",
                "",
                f"- delivery_id: `{event.delivery_id}`",
                f"- upstream_ref: `{upstream_ref}`",
                f"- upstream_after: `{event.after_sha}`",
                f"- artifact: `{artifact_path}`",
                "",
                "Please port missing surface items and close this issue once parity is restored.",
            ]
        )
        created = gh_issue_create(repo_root, title, issue_body, parity_labels)
        if created:
            result["created_issue_url"] = created
            seen.setdefault("fingerprints", {})[fingerprint] = {
                "created_at": generated_at,
                "issue": created,
                "delivery_id": event.delivery_id,
            }
            save_seen_drift_fingerprints(seen_path, seen)
    return result


def run_sync_for_event(
    event: SyncEvent,
    repo_root: str,
    report_dir: str,
    strategy: str,
    strict_risk_gate: bool,
    allow_risk_paths: bool,
    conflict_label: str,
    skip_tests: bool,
    no_pr: bool,
    draft_pr: bool,
    pr_labels: str,
    run_redteam_gate: bool,
    redteam_cmd: str,
    timeout_sec: int,
) -> tuple[int, str, str]:
    sync_script = os.path.join(repo_root, "scripts", "sync-upstream.sh")
    cmd = [
        "bash",
        sync_script,
        "--repo-root",
        repo_root,
        "--mode",
        "branch-pr",
        "--strategy",
        strategy,
        "--report-dir",
        report_dir,
        "--conflict-label",
        conflict_label,
    ]
    if strict_risk_gate:
        cmd.append("--strict-risk-gate")
    else:
        cmd.append("--no-strict-risk-gate")
    if allow_risk_paths:
        cmd.append("--allow-risk-paths")
    if skip_tests:
        cmd.append("--no-tests")
    if run_redteam_gate:
        cmd.append("--redteam-gate")
    else:
        cmd.append("--no-redteam-gate")
    if redteam_cmd.strip():
        cmd.extend(["--redteam-cmd", redteam_cmd.strip()])
    if no_pr:
        cmd.append("--no-pr")
    if draft_pr:
        cmd.append("--draft-pr")
    if pr_labels.strip():
        cmd.extend(["--pr-labels", pr_labels.strip()])

    LOG.info(
        "running sync for delivery=%s repo=%s ref=%s after=%s",
        event.delivery_id,
        event.repository,
        event.ref,
        event.after_sha[:12],
    )
    proc = subprocess.run(
        cmd,
        cwd=repo_root,
        text=True,
        capture_output=True,
        timeout=max(60, timeout_sec),
    )
    output = (proc.stdout or "") + "\n" + (proc.stderr or "")
    report_path = ""
    for line in output.splitlines():
        line = line.strip()
        if line.startswith("[sync-upstream] Report: "):
            report_path = line.split("Report:", 1)[1].strip()
    if not report_path:
        # Fallback to latest report in directory.
        rp = pathlib.Path(report_dir)
        reports = sorted(rp.glob("upstream-sync-*.txt"), key=lambda p: p.stat().st_mtime)
        if reports:
            report_path = str(reports[-1])
    return proc.returncode, output, report_path


def maybe_run_assist_hook(
    assist_cmd: str | None,
    event: SyncEvent,
    report_path: str,
    outcome: str,
    repo_root: str,
) -> None:
    if not assist_cmd:
        return
    env = os.environ.copy()
    env["UPSTREAM_SYNC_DELIVERY_ID"] = event.delivery_id
    env["UPSTREAM_SYNC_REPOSITORY"] = event.repository
    env["UPSTREAM_SYNC_REF"] = event.ref
    env["UPSTREAM_SYNC_AFTER_SHA"] = event.after_sha
    env["UPSTREAM_SYNC_OUTCOME"] = outcome
    env["UPSTREAM_SYNC_REPORT_PATH"] = report_path
    try:
        subprocess.run(
            ["bash", "-lc", assist_cmd],
            cwd=repo_root,
            env=env,
            timeout=120,
            check=False,
            text=True,
            capture_output=True,
        )
    except Exception as exc:  # pragma: no cover
        LOG.warning("assist hook failed: %s", exc)


def serve_webhook(args: argparse.Namespace) -> int:
    expected_repo = args.expected_repo
    expected_ref = args.expected_ref
    secret = args.webhook_secret or os.environ.get("GITHUB_WEBHOOK_SECRET", "")

    if args.backend == "sqlite":
        queue = SqliteQueue(args.sqlite_path)
        enqueue = queue.enqueue
    elif args.backend == "sqs":
        pub = SqsPublisher(args.sqs_queue_url, args.sqs_region)
        enqueue = pub.enqueue
    else:
        pub = KafkaPublisher(args.kafka_bootstrap, args.kafka_topic)
        enqueue = pub.enqueue

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802
            if self.path != args.path:
                self.send_error(HTTPStatus.NOT_FOUND)
                return

            content_len = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(content_len)
            sig = self.headers.get("X-Hub-Signature-256")
            delivery_id = self.headers.get("X-GitHub-Delivery", "")
            event_name = self.headers.get("X-GitHub-Event", "")

            if not verify_github_signature(body, secret, sig):
                self.send_error(HTTPStatus.UNAUTHORIZED, "invalid signature")
                return

            if event_name != "push":
                self._respond_json({"accepted": False, "reason": "ignored_event"})
                return

            if not delivery_id:
                self.send_error(HTTPStatus.BAD_REQUEST, "missing delivery id")
                return

            try:
                payload = json.loads(body.decode("utf-8"))
            except Exception:
                self.send_error(HTTPStatus.BAD_REQUEST, "invalid json")
                return

            event = make_event_from_payload(delivery_id, event_name, payload)
            if event.repository != expected_repo or event.ref != expected_ref:
                self._respond_json(
                    {
                        "accepted": False,
                        "reason": "repo_or_ref_mismatch",
                        "repository": event.repository,
                        "ref": event.ref,
                    }
                )
                return

            inserted = enqueue(event)
            self._respond_json(
                {
                    "accepted": True,
                    "queued": inserted,
                    "delivery_id": event.delivery_id,
                    "after_sha": event.after_sha,
                    "repository": event.repository,
                    "ref": event.ref,
                }
            )

        def log_message(self, format: str, *args: Any) -> None:  # noqa: A003
            LOG.info("%s - %s", self.address_string(), format % args)

        def _respond_json(self, payload: dict[str, Any]) -> None:
            body = json.dumps(payload).encode("utf-8")
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    LOG.info(
        "webhook server listening on http://%s:%s%s backend=%s expected=%s %s",
        args.host,
        args.port,
        args.path,
        args.backend,
        expected_repo,
        expected_ref,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        LOG.info("webhook server interrupted; exiting")
    finally:
        server.server_close()
    return 0


def worker_loop(args: argparse.Namespace) -> int:
    repo_root = args.repo_root
    report_dir = os.path.join(repo_root, ".sync-reports")
    os.makedirs(report_dir, exist_ok=True)
    parity_labels = [
        v.strip() for v in str(args.parity_labels).split(",") if v.strip()
    ]
    parity_drift_enabled = not bool(args.disable_parity_drift_check)
    open_parity_issue = not bool(args.no_parity_open_issues)

    assist_cmd = args.assist_cmd or os.environ.get("UPSTREAM_SYNC_ASSIST_CMD", "")
    assist_cmd = assist_cmd.strip() or None

    if args.backend == "sqlite":
        queue = SqliteQueue(args.sqlite_path)
        while True:
            item = queue.claim_next()
            if item is None:
                time.sleep(args.poll_interval_sec)
                continue
            rc, output, report_path = run_sync_for_event(
                event=item.event,
                repo_root=repo_root,
                report_dir=report_dir,
                strategy=args.strategy,
                strict_risk_gate=args.strict_risk_gate,
                allow_risk_paths=args.allow_risk_paths,
                conflict_label=args.conflict_label,
                skip_tests=args.no_tests,
                no_pr=args.no_pr,
                draft_pr=args.draft_pr,
                pr_labels=args.pr_labels,
                run_redteam_gate=not args.no_redteam_gate,
                redteam_cmd=args.redteam_cmd,
                timeout_sec=args.sync_timeout_sec,
            )
            outcome = parse_report_status(report_path)
            if rc == 0:
                upstream_ref = (
                    item.event.after_sha
                    if item.event.after_sha and git_ref_exists(repo_root, item.event.after_sha)
                    else args.parity_upstream_ref
                )
                drift_result = maybe_check_cli_surface_drift(
                    repo_root=repo_root,
                    report_dir=report_dir,
                    event=item.event,
                    upstream_ref=upstream_ref,
                    parent_issue=args.parity_parent_issue,
                    open_parity_issue=open_parity_issue,
                    parity_labels=parity_labels,
                    enabled=parity_drift_enabled,
                )
                global_drift_result = maybe_check_global_parity_drift(
                    repo_root=repo_root,
                    report_dir=report_dir,
                    event=item.event,
                    parent_issue=args.global_parity_parent_issue,
                    open_parity_issue=(not args.no_global_parity_open_issues),
                    parity_labels=[
                        v.strip() for v in str(args.global_parity_labels).split(",") if v.strip()
                    ],
                    enabled=(not args.disable_global_parity_check),
                    max_queue_commits=args.global_parity_max_queue_commits,
                )
                note = (
                    f"{outcome}; cli_drift={'detected' if drift_result.get('has_drift') else 'none'}; "
                    f"global_drift={'detected' if global_drift_result.get('has_drift') else 'none'}"
                )
                if drift_result.get("created_issue_url"):
                    note += f"; issue={drift_result['created_issue_url']}"
                if global_drift_result.get("created_issue_url"):
                    note += f"; global_issue={global_drift_result['created_issue_url']}"
                queue.mark_done(item.id, status="done", note=note)
                continue

            if outcome in {"risk_blocked", "conflict"}:
                queue.mark_done(item.id, status=outcome, note=outcome)
                maybe_run_assist_hook(assist_cmd, item.event, report_path, outcome, repo_root)
                continue

            queue.mark_retry_or_dead(
                item.id, attempts=item.attempts, max_attempts=args.max_attempts, error=output[-1000:]
            )
            if item.attempts >= args.max_attempts:
                maybe_run_assist_hook(
                    assist_cmd, item.event, report_path, "dead", repo_root
                )
        return 0

    if args.backend == "sqs":
        consumer = SqsConsumer(args.sqs_queue_url, args.sqs_region)
        while True:
            message = consumer.receive(wait_seconds=20)
            if message is None:
                continue
            event, receipt_handle = message
            rc, output, report_path = run_sync_for_event(
                event=event,
                repo_root=repo_root,
                report_dir=report_dir,
                strategy=args.strategy,
                strict_risk_gate=args.strict_risk_gate,
                allow_risk_paths=args.allow_risk_paths,
                conflict_label=args.conflict_label,
                skip_tests=args.no_tests,
                no_pr=args.no_pr,
                draft_pr=args.draft_pr,
                pr_labels=args.pr_labels,
                run_redteam_gate=not args.no_redteam_gate,
                redteam_cmd=args.redteam_cmd,
                timeout_sec=args.sync_timeout_sec,
            )
            outcome = parse_report_status(report_path)
            if rc == 0:
                upstream_ref = (
                    event.after_sha
                    if event.after_sha and git_ref_exists(repo_root, event.after_sha)
                    else args.parity_upstream_ref
                )
                drift_result = maybe_check_cli_surface_drift(
                    repo_root=repo_root,
                    report_dir=report_dir,
                    event=event,
                    upstream_ref=upstream_ref,
                    parent_issue=args.parity_parent_issue,
                    open_parity_issue=open_parity_issue,
                    parity_labels=parity_labels,
                    enabled=parity_drift_enabled,
                )
                global_drift_result = maybe_check_global_parity_drift(
                    repo_root=repo_root,
                    report_dir=report_dir,
                    event=event,
                    parent_issue=args.global_parity_parent_issue,
                    open_parity_issue=(not args.no_global_parity_open_issues),
                    parity_labels=[
                        v.strip() for v in str(args.global_parity_labels).split(",") if v.strip()
                    ],
                    enabled=(not args.disable_global_parity_check),
                    max_queue_commits=args.global_parity_max_queue_commits,
                )
                if drift_result.get("has_drift"):
                    LOG.warning(
                        "CLI surface drift detected for delivery=%s artifact=%s",
                        event.delivery_id,
                        drift_result.get("artifact_path", ""),
                    )
                if global_drift_result.get("has_drift"):
                    LOG.warning(
                        "Global parity drift detected for delivery=%s artifact=%s",
                        event.delivery_id,
                        global_drift_result.get("artifact_path", ""),
                    )
            # In SQS mode: acknowledge terminal states. Let visibility timeout retry transient failures.
            if rc == 0 or outcome in {"risk_blocked", "conflict"}:
                consumer.ack(receipt_handle)
            if outcome in {"risk_blocked", "conflict"}:
                maybe_run_assist_hook(assist_cmd, event, report_path, outcome, repo_root)
            elif rc != 0:
                LOG.warning("sync failed for delivery=%s; will retry via visibility timeout", event.delivery_id)
        return 0

    # kafka
    consumer = KafkaConsumerWrapper(
        bootstrap=args.kafka_bootstrap,
        topic=args.kafka_topic,
        group_id=args.kafka_group_id,
    )
    while True:
        message = consumer.poll(timeout_ms=1000)
        if message is None:
            continue
        event, _rec = message
        rc, _output, report_path = run_sync_for_event(
            event=event,
            repo_root=repo_root,
            report_dir=report_dir,
            strategy=args.strategy,
            strict_risk_gate=args.strict_risk_gate,
            allow_risk_paths=args.allow_risk_paths,
            conflict_label=args.conflict_label,
            skip_tests=args.no_tests,
            no_pr=args.no_pr,
            draft_pr=args.draft_pr,
            pr_labels=args.pr_labels,
            run_redteam_gate=not args.no_redteam_gate,
            redteam_cmd=args.redteam_cmd,
            timeout_sec=args.sync_timeout_sec,
        )
        outcome = parse_report_status(report_path)
        if rc == 0:
            upstream_ref = (
                event.after_sha
                if event.after_sha and git_ref_exists(repo_root, event.after_sha)
                else args.parity_upstream_ref
            )
            drift_result = maybe_check_cli_surface_drift(
                repo_root=repo_root,
                report_dir=report_dir,
                event=event,
                upstream_ref=upstream_ref,
                parent_issue=args.parity_parent_issue,
                open_parity_issue=open_parity_issue,
                parity_labels=parity_labels,
                enabled=parity_drift_enabled,
            )
            global_drift_result = maybe_check_global_parity_drift(
                repo_root=repo_root,
                report_dir=report_dir,
                event=event,
                parent_issue=args.global_parity_parent_issue,
                open_parity_issue=(not args.no_global_parity_open_issues),
                parity_labels=[
                    v.strip() for v in str(args.global_parity_labels).split(",") if v.strip()
                ],
                enabled=(not args.disable_global_parity_check),
                max_queue_commits=args.global_parity_max_queue_commits,
            )
            if drift_result.get("has_drift"):
                LOG.warning(
                    "CLI surface drift detected for delivery=%s artifact=%s",
                    event.delivery_id,
                    drift_result.get("artifact_path", ""),
                )
            if global_drift_result.get("has_drift"):
                LOG.warning(
                    "Global parity drift detected for delivery=%s artifact=%s",
                    event.delivery_id,
                    global_drift_result.get("artifact_path", ""),
                )
        # Commit offsets for terminal states; for transient failures we keep offset
        # uncommitted so another worker run can retry.
        if rc == 0 or outcome in {"risk_blocked", "conflict"}:
            consumer.ack()
        if outcome in {"risk_blocked", "conflict"}:
            maybe_run_assist_hook(assist_cmd, event, report_path, outcome, repo_root)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Webhook-driven upstream sync")
    parser.add_argument("--log-level", default="INFO", help="Logging level (default: INFO)")
    sub = parser.add_subparsers(dest="cmd", required=True)

    listen = sub.add_parser("listen", help="Start GitHub webhook receiver")
    listen.add_argument("--host", default="127.0.0.1")
    listen.add_argument("--port", type=int, default=8099)
    listen.add_argument("--path", default="/github/upstream-sync")
    listen.add_argument("--webhook-secret", default="", help="GitHub webhook secret")
    listen.add_argument("--expected-repo", default=DEFAULT_UPSTREAM_REPO)
    listen.add_argument("--expected-ref", default=DEFAULT_UPSTREAM_REF)
    listen.add_argument("--backend", choices=["sqlite", "sqs", "kafka"], default="sqlite")
    listen.add_argument("--sqlite-path", default=".sync-queue/upstream-events.db")
    listen.add_argument("--sqs-queue-url", default=os.environ.get("UPSTREAM_SYNC_SQS_QUEUE_URL", ""))
    listen.add_argument("--sqs-region", default=os.environ.get("AWS_REGION", ""))
    listen.add_argument("--kafka-bootstrap", default=os.environ.get("UPSTREAM_SYNC_KAFKA_BOOTSTRAP", "127.0.0.1:9092"))
    listen.add_argument("--kafka-topic", default=os.environ.get("UPSTREAM_SYNC_KAFKA_TOPIC", "hermes-upstream-sync"))

    worker = sub.add_parser("worker", help="Run queue worker and execute sync")
    worker.add_argument("--repo-root", default=str(pathlib.Path(__file__).resolve().parent.parent))
    worker.add_argument("--backend", choices=["sqlite", "sqs", "kafka"], default="sqlite")
    worker.add_argument("--sqlite-path", default=".sync-queue/upstream-events.db")
    worker.add_argument("--sqs-queue-url", default=os.environ.get("UPSTREAM_SYNC_SQS_QUEUE_URL", ""))
    worker.add_argument("--sqs-region", default=os.environ.get("AWS_REGION", ""))
    worker.add_argument("--kafka-bootstrap", default=os.environ.get("UPSTREAM_SYNC_KAFKA_BOOTSTRAP", "127.0.0.1:9092"))
    worker.add_argument("--kafka-topic", default=os.environ.get("UPSTREAM_SYNC_KAFKA_TOPIC", "hermes-upstream-sync"))
    worker.add_argument("--kafka-group-id", default=os.environ.get("UPSTREAM_SYNC_KAFKA_GROUP", "hermes-upstream-worker"))
    worker.add_argument("--poll-interval-sec", type=int, default=10)
    worker.add_argument("--max-attempts", type=int, default=3)
    worker.add_argument("--sync-timeout-sec", type=int, default=1800)
    worker.add_argument("--strategy", choices=["merge", "cherry-pick"], default="merge")
    worker.add_argument("--strict-risk-gate", action="store_true", default=True)
    worker.add_argument("--allow-risk-paths", action="store_true", default=False)
    worker.add_argument("--conflict-label", default="upstream-sync-conflict")
    worker.add_argument("--no-tests", action="store_true", default=False)
    worker.add_argument(
        "--no-redteam-gate",
        action="store_true",
        default=False,
        help="Skip adversarial red-team gate during sync runs.",
    )
    worker.add_argument(
        "--redteam-cmd",
        default=os.environ.get("UPSTREAM_SYNC_REDTEAM_CMD", "python3 scripts/run-redteam-gate.py"),
        help="Command used for adversarial red-team gate.",
    )
    worker.add_argument("--no-pr", action="store_true", default=False)
    worker.add_argument(
        "--draft-pr",
        action="store_true",
        default=False,
        help="Open sync PRs as draft (ignored when --no-pr is set)",
    )
    worker.add_argument(
        "--pr-labels",
        default=os.environ.get("UPSTREAM_SYNC_PR_LABELS", "upstream-sync,parity-sync"),
        help="Comma-separated labels applied to created sync PRs.",
    )
    worker.add_argument(
        "--disable-parity-drift-check",
        action="store_true",
        default=False,
        help="Disable CLI command/action drift detection against upstream ref",
    )
    worker.add_argument(
        "--parity-upstream-ref",
        default="upstream/main",
        help="Git ref used for parity drift comparison (default: upstream/main)",
    )
    worker.add_argument(
        "--parity-parent-issue",
        type=int,
        default=13,
        help="Issue number to comment with drift reports (default: 13)",
    )
    worker.add_argument(
        "--parity-labels",
        default="parity,parity-upkeep",
        help="Comma-separated labels for auto-created drift issues",
    )
    worker.add_argument(
        "--no-parity-open-issues",
        action="store_true",
        default=False,
        help="Do not auto-open new drift issues when new fingerprints are found",
    )
    worker.add_argument(
        "--disable-global-parity-check",
        action="store_true",
        default=False,
        help="Disable global parity gate audit after successful upstream sync",
    )
    worker.add_argument(
        "--global-parity-parent-issue",
        type=int,
        default=19,
        help="Issue number to comment with global parity drift reports (default: 19)",
    )
    worker.add_argument(
        "--global-parity-labels",
        default="parity,parity-upkeep",
        help="Comma-separated labels for global parity drift issues",
    )
    worker.add_argument(
        "--no-global-parity-open-issues",
        action="store_true",
        default=False,
        help="Do not auto-open new global parity drift issues for new fingerprints",
    )
    worker.add_argument(
        "--global-parity-max-queue-commits",
        type=int,
        default=0,
        help="Max commits for generated upstream queue artifact (0 means full range)",
    )
    worker.add_argument(
        "--assist-cmd",
        default="",
        help="Optional command to run on risk_blocked/conflict/dead outcomes (Nous/Codex helper hook)",
    )

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    logging.basicConfig(
        level=getattr(logging, str(args.log_level).upper(), logging.INFO),
        format="[%(asctime)s] %(levelname)s %(name)s: %(message)s",
    )

    if args.cmd == "listen":
        if args.backend == "sqs" and not args.sqs_queue_url:
            parser.error("--sqs-queue-url is required for SQS backend")
        if args.backend == "kafka" and (not args.kafka_bootstrap or not args.kafka_topic):
            parser.error("--kafka-bootstrap and --kafka-topic are required for Kafka backend")
        return serve_webhook(args)

    if args.cmd == "worker":
        if args.backend == "sqs" and not args.sqs_queue_url:
            parser.error("--sqs-queue-url is required for SQS backend")
        if args.backend == "kafka" and (not args.kafka_bootstrap or not args.kafka_topic):
            parser.error("--kafka-bootstrap and --kafka-topic are required for Kafka backend")
        return worker_loop(args)

    parser.error(f"unsupported command: {args.cmd}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
