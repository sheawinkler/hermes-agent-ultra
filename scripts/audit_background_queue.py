#!/usr/bin/env python3
"""Audit and optionally repair Hermes background job queue manifests."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import pathlib
import sys
from typing import Any


ACTIVE_STATUSES = {"queued", "running"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--home", default="", help="Hermes home directory override")
    parser.add_argument(
        "--stale-running-seconds",
        type=int,
        default=1800,
        help="Mark running jobs older than this threshold as stale",
    )
    parser.add_argument("--repair", action="store_true", help="Apply safe repairs")
    parser.add_argument(
        "--report",
        default="",
        help="Optional report output path (defaults to .sync-reports/background-queue-audit-*.json)",
    )
    parser.add_argument("--json", action="store_true", help="Print report JSON")
    return parser.parse_args()


def resolve_home(args: argparse.Namespace) -> pathlib.Path:
    if args.home:
        return pathlib.Path(args.home).expanduser().resolve()
    env_home = (pathlib.Path.home() / ".hermes-agent-ultra").resolve()
    return env_home


def parse_time(value: Any) -> dt.datetime | None:
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except Exception:
        return None


def job_key(task: str) -> str:
    return hashlib.sha256(task.strip().encode("utf-8")).hexdigest()[:16]


def load_job(path: pathlib.Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        raw = path.read_text(encoding="utf-8")
    except Exception as exc:
        return None, f"read_error: {exc}"
    try:
        parsed = json.loads(raw)
        if not isinstance(parsed, dict):
            return None, "not_an_object"
        return parsed, None
    except Exception as exc:
        return None, f"json_error: {exc}"


def write_job(path: pathlib.Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"background-queue-audit-{stamp}.json"


def main() -> int:
    args = parse_args()
    home = resolve_home(args)
    jobs_dir = home / "background_jobs"
    repo_root = pathlib.Path.cwd()

    findings: dict[str, Any] = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "home": str(home),
        "jobs_dir": str(jobs_dir),
        "repair": args.repair,
        "totals": {
            "files": 0,
            "malformed": 0,
            "stale_running": 0,
            "duplicate_active": 0,
            "repaired": 0,
        },
        "malformed": [],
        "stale_running": [],
        "duplicate_active": [],
    }

    if not jobs_dir.exists():
        findings["note"] = "jobs directory not found"
        findings["ok"] = True
    else:
        now = dt.datetime.now(dt.timezone.utc)
        active_by_task: dict[str, list[tuple[pathlib.Path, dict[str, Any]]]] = {}
        for path in sorted(jobs_dir.glob("*.json")):
            findings["totals"]["files"] += 1
            payload, err = load_job(path)
            if err:
                findings["totals"]["malformed"] += 1
                findings["malformed"].append({"path": str(path), "error": err})
                continue

            status = str(payload.get("status", "")).strip().lower()
            task = str(payload.get("task", "")).strip()
            started_at = parse_time(payload.get("started_at"))

            if status == "running" and started_at is not None:
                age = (now - started_at).total_seconds()
                if age > args.stale_running_seconds:
                    findings["totals"]["stale_running"] += 1
                    entry = {
                        "path": str(path),
                        "status": status,
                        "age_seconds": int(age),
                    }
                    findings["stale_running"].append(entry)
                    if args.repair:
                        payload["status"] = "failed"
                        payload["finished_at"] = now.isoformat()
                        payload["error"] = f"queue_audit_repair: stale running job (> {args.stale_running_seconds}s)"
                        write_job(path, payload)
                        findings["totals"]["repaired"] += 1

            if status in ACTIVE_STATUSES and task:
                active_by_task.setdefault(job_key(task), []).append((path, payload))

        for key, rows in active_by_task.items():
            if len(rows) <= 1:
                continue
            rows.sort(key=lambda item: str(item[1].get("created_at", "")))
            keep_path, _ = rows[0]
            dup_paths = [str(path) for path, _ in rows[1:]]
            findings["totals"]["duplicate_active"] += len(dup_paths)
            findings["duplicate_active"].append(
                {
                    "task_key": key,
                    "keep": str(keep_path),
                    "duplicates": dup_paths,
                }
            )
            if args.repair:
                for dup_path, dup_payload in rows[1:]:
                    dup_payload["status"] = "failed"
                    dup_payload["finished_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
                    dup_payload["error"] = f"queue_audit_repair: duplicate active task of {keep_path.name}"
                    write_job(dup_path, dup_payload)
                    findings["totals"]["repaired"] += 1

        findings["ok"] = (
            findings["totals"]["malformed"] == 0
            and findings["totals"]["stale_running"] == 0
            and findings["totals"]["duplicate_active"] == 0
        )

    report_path = pathlib.Path(args.report).expanduser().resolve() if args.report else default_report_path(repo_root)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(findings, indent=2) + "\n", encoding="utf-8")
    findings["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(findings, indent=2))
    else:
        print(
            "[background-queue-audit] "
            f"{'PASS' if findings.get('ok') else 'WARN'} "
            f"files={findings['totals']['files']} malformed={findings['totals']['malformed']} "
            f"stale={findings['totals']['stale_running']} duplicates={findings['totals']['duplicate_active']} "
            f"repaired={findings['totals']['repaired']}"
        )
        print(f"[background-queue-audit] Report: {report_path}")

    return 0 if findings.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
