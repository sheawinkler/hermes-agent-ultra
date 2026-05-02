#!/usr/bin/env python3
"""Publish automation summaries with ContextLattice-first sink fallback."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from typing import Any

DEFAULT_CONTEXTLATTICE_URL = "http://127.0.0.1:8075"
DEFAULT_SINK_ORDER = "contextlattice,github,local"
SUPPORTED_SINKS = {"contextlattice", "github", "local", "stdout"}


def utc_now_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--summary-kind",
        default=os.environ.get("AUTOMATION_SUMMARY_KIND", "automation"),
        help="Summary kind label",
    )
    parser.add_argument(
        "--status",
        default=os.environ.get("AUTOMATION_SUMMARY_STATUS", "unknown"),
        help="Run status label",
    )
    parser.add_argument(
        "--title",
        default=os.environ.get("AUTOMATION_SUMMARY_TITLE", ""),
        help="Optional summary title",
    )
    parser.add_argument(
        "--summary-body",
        default="",
        help="Inline summary markdown/text body",
    )
    parser.add_argument(
        "--summary-body-file",
        default="",
        help="Path to summary markdown/text body file",
    )
    parser.add_argument(
        "--metadata-json",
        default="",
        help="Inline JSON object merged into summary metadata",
    )
    parser.add_argument(
        "--metadata-file",
        default="",
        help="Path to JSON object merged into summary metadata",
    )
    parser.add_argument(
        "--sink-order",
        default=os.environ.get("AUTOMATION_SUMMARY_SINK_ORDER", DEFAULT_SINK_ORDER),
        help="Comma-separated sink order: contextlattice,github,local,stdout",
    )
    parser.add_argument(
        "--contextlattice-url",
        default=os.environ.get("CONTEXTLATTICE_ORCHESTRATOR_URL")
        or os.environ.get("MEMMCP_ORCHESTRATOR_URL")
        or DEFAULT_CONTEXTLATTICE_URL,
        help="ContextLattice orchestrator base URL",
    )
    parser.add_argument(
        "--context-project",
        default=os.environ.get("AUTOMATION_SUMMARY_CONTEXT_PROJECT", ""),
        help="ContextLattice project name (default: repo dir name)",
    )
    parser.add_argument(
        "--context-topic-path",
        default=os.environ.get(
            "AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH",
            "ops/automation-summary",
        ),
        help="ContextLattice topic path for writes",
    )
    parser.add_argument(
        "--context-file-name",
        default=os.environ.get(
            "AUTOMATION_SUMMARY_CONTEXT_FILE_NAME",
            "ops/automation-summary.md",
        ),
        help="ContextLattice file key for writes",
    )
    parser.add_argument(
        "--context-agent-id",
        default=os.environ.get("AUTOMATION_SUMMARY_CONTEXT_AGENT_ID", "hermes_ultra_automation"),
        help="Agent id recorded in summary metadata",
    )
    parser.add_argument(
        "--context-timeout-secs",
        type=float,
        default=float(os.environ.get("AUTOMATION_SUMMARY_CONTEXT_TIMEOUT_SECS", "8")),
        help="ContextLattice write timeout seconds",
    )
    parser.add_argument(
        "--context-api-key",
        default=os.environ.get("AUTOMATION_SUMMARY_CONTEXT_API_KEY")
        or os.environ.get("CONTEXTLATTICE_API_KEY")
        or "",
        help="Optional ContextLattice API key",
    )
    parser.add_argument(
        "--github-issue",
        type=int,
        default=int(os.environ.get("AUTOMATION_SUMMARY_GITHUB_ISSUE", "0") or "0"),
        help="Fallback GitHub issue number for comment sink",
    )
    parser.add_argument(
        "--local-path",
        default=os.environ.get("AUTOMATION_SUMMARY_LOCAL_PATH", ""),
        help="Fallback local file path (default: <repo>/.sync-reports/automation-summary-fallback.log)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit result as JSON",
    )
    return parser.parse_args()


def parse_sink_order(raw: str) -> list[str]:
    order: list[str] = []
    for token in str(raw or "").split(","):
        sink = token.strip().lower()
        if not sink:
            continue
        if sink not in SUPPORTED_SINKS:
            continue
        if sink not in order:
            order.append(sink)
    if not order:
        return ["local"]
    return order


def load_json_object(raw_json: str, path: str) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    if path:
        try:
            parsed = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
            if isinstance(parsed, dict):
                payload.update(parsed)
        except Exception:
            pass
    if raw_json:
        try:
            parsed = json.loads(raw_json)
            if isinstance(parsed, dict):
                payload.update(parsed)
        except Exception:
            pass
    return payload


def build_summary_markdown(
    *,
    title: str,
    summary_kind: str,
    status: str,
    metadata: dict[str, Any],
    body: str,
) -> str:
    heading = title.strip() or f"{summary_kind.strip() or 'automation'} summary"
    lines = [f"## {heading}", ""]
    keys = ["timestamp_utc", "status", "summary_kind", "repo", "head_sha", "delivery_id"]
    for key in keys:
        value = metadata.get(key)
        if value is None or value == "":
            continue
        lines.append(f"- {key}: `{value}`")
    extra = {
        key: value
        for key, value in metadata.items()
        if key not in keys
        and value is not None
        and value != ""
    }
    if extra:
        lines.append(f"- metadata: `{json.dumps(extra, sort_keys=True)}`")
    lines.extend(["", body.strip() if body.strip() else "_No summary body provided._", ""])
    return "\n".join(lines)


def write_contextlattice(
    *,
    url: str,
    timeout_secs: float,
    api_key: str,
    project: str,
    file_name: str,
    topic_path: str,
    content: str,
) -> tuple[bool, str]:
    endpoint = url.rstrip("/") + "/memory/write"
    payload: dict[str, Any] = {
        "projectName": project,
        "fileName": file_name,
        "content": content,
    }
    if topic_path:
        payload["topicPath"] = topic_path
    req = urllib.request.Request(
        endpoint,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
        method="POST",
    )
    if api_key.strip():
        req.add_header("Authorization", f"Bearer {api_key.strip()}")
    try:
        with urllib.request.urlopen(req, timeout=max(0.5, timeout_secs)) as resp:
            status = getattr(resp, "status", 200)
            if int(status) >= 300:
                return False, f"http_status={status}"
            return True, f"http_status={status}"
    except urllib.error.HTTPError as exc:
        return False, f"http_status={exc.code}"
    except Exception as exc:
        return False, f"error={type(exc).__name__}:{exc}"


def comment_github_issue(
    *,
    repo_root: pathlib.Path,
    issue: int,
    content: str,
) -> tuple[bool, str]:
    if issue <= 0:
        return False, "issue_not_configured"
    if shutil.which("gh") is None:
        return False, "gh_cli_missing"
    proc = subprocess.run(
        ["gh", "issue", "comment", str(issue), "--body-file", "-"],
        cwd=str(repo_root),
        input=content,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return False, proc.stderr.strip() or f"gh_exit_{proc.returncode}"
    return True, "commented"


def append_local_log(path: pathlib.Path, content: str) -> tuple[bool, str]:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        stamp = utc_now_iso()
        with path.open("a", encoding="utf-8") as fh:
            fh.write(f"\n\n===== {stamp} =====\n")
            fh.write(content)
            if not content.endswith("\n"):
                fh.write("\n")
        return True, str(path)
    except Exception as exc:
        return False, f"error={type(exc).__name__}:{exc}"


def publish_summary(
    *,
    sink_order: list[str],
    summary_markdown: str,
    repo_root: pathlib.Path,
    contextlattice_url: str,
    context_timeout_secs: float,
    context_api_key: str,
    context_project: str,
    context_file_name: str,
    context_topic_path: str,
    github_issue: int,
    local_path: pathlib.Path,
) -> dict[str, Any]:
    attempts: list[dict[str, str]] = []
    for sink in sink_order:
        if sink == "contextlattice":
            ok, detail = write_contextlattice(
                url=contextlattice_url,
                timeout_secs=context_timeout_secs,
                api_key=context_api_key,
                project=context_project,
                file_name=context_file_name,
                topic_path=context_topic_path,
                content=summary_markdown,
            )
        elif sink == "github":
            ok, detail = comment_github_issue(
                repo_root=repo_root,
                issue=github_issue,
                content=summary_markdown,
            )
        elif sink == "local":
            ok, detail = append_local_log(local_path, summary_markdown)
        elif sink == "stdout":
            print(summary_markdown)
            ok, detail = True, "printed_stdout"
        else:
            ok, detail = False, "unsupported_sink"

        attempts.append({"sink": sink, "ok": "true" if ok else "false", "detail": detail})
        if ok:
            return {
                "ok": True,
                "primary_sink": sink,
                "attempts": attempts,
            }
    return {
        "ok": False,
        "primary_sink": "",
        "attempts": attempts,
    }


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    summary_body = ""
    if args.summary_body_file:
        summary_body = pathlib.Path(args.summary_body_file).expanduser().read_text(
            encoding="utf-8"
        )
    if args.summary_body:
        summary_body = args.summary_body

    metadata = load_json_object(args.metadata_json, args.metadata_file)
    metadata.setdefault("timestamp_utc", dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))
    metadata.setdefault("status", args.status)
    metadata.setdefault("summary_kind", args.summary_kind)
    metadata.setdefault("repo", str(repo_root))
    metadata.setdefault("agent_id", args.context_agent_id)

    context_project = (
        args.context_project.strip() if args.context_project.strip() else repo_root.name
    )
    summary_markdown = build_summary_markdown(
        title=args.title,
        summary_kind=args.summary_kind,
        status=args.status,
        metadata=metadata,
        body=summary_body,
    )
    sink_order = parse_sink_order(args.sink_order)
    local_path = (
        pathlib.Path(args.local_path).expanduser()
        if args.local_path
        else repo_root / ".sync-reports" / "automation-summary-fallback.log"
    )
    result = publish_summary(
        sink_order=sink_order,
        summary_markdown=summary_markdown,
        repo_root=repo_root,
        contextlattice_url=args.contextlattice_url,
        context_timeout_secs=args.context_timeout_secs,
        context_api_key=args.context_api_key,
        context_project=context_project,
        context_file_name=args.context_file_name,
        context_topic_path=args.context_topic_path,
        github_issue=args.github_issue,
        local_path=local_path,
    )
    result_payload = {
        "ok": result["ok"],
        "primary_sink": result["primary_sink"],
        "attempts": result["attempts"],
        "metadata": metadata,
        "context": {
            "project": context_project,
            "topic_path": args.context_topic_path,
            "file_name": args.context_file_name,
            "orchestrator_url": args.contextlattice_url,
        },
        "local_path": str(local_path),
    }
    if args.json:
        print(json.dumps(result_payload, indent=2, sort_keys=True))
    else:
        status = "ok" if result["ok"] else "failed"
        print(
            f"[publish-automation-summary] {status} "
            f"sink={result['primary_sink'] or '<none>'} "
            f"attempts={len(result['attempts'])}"
        )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
