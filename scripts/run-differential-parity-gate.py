#!/usr/bin/env python3
"""Run a focused CLI-surface parity drift gate against upstream."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import subprocess
import sys
import time
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--upstream-ref",
        default="upstream/main",
        help="Upstream git ref for parity comparison",
    )
    parser.add_argument(
        "--local-ref",
        default="HEAD",
        help="Local git ref to evaluate (default HEAD)",
    )
    parser.add_argument(
        "--max-commits-behind",
        type=int,
        default=0,
        help="Maximum allowed commits behind upstream",
    )
    parser.add_argument(
        "--fallback-cmd",
        default="python3 scripts/generate-global-parity-proof.py --check-ci",
        help="Fallback command when CLI surface extraction is unavailable for compared refs",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit JSON report path",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def run(cmd: list[str], cwd: pathlib.Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )


def run_shell(command: str, cwd: pathlib.Path) -> dict[str, Any]:
    started = time.time()
    proc = subprocess.run(
        ["bash", "-lc", command],
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )
    elapsed_ms = int((time.time() - started) * 1000)
    return {
        "command": command,
        "exit_code": proc.returncode,
        "ok": proc.returncode == 0,
        "elapsed_ms": elapsed_ms,
        "stdout_tail": (proc.stdout or "")[-4000:],
        "stderr_tail": (proc.stderr or "")[-4000:],
    }


def git_show_file(repo_root: pathlib.Path, ref: str, path: str) -> str | None:
    proc = run(["git", "show", f"{ref}:{path}"], repo_root)
    if proc.returncode != 0:
        return None
    return proc.stdout


def rust_fn_block(source: str, fn_name: str) -> str:
    m = re.search(rf"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+{re.escape(fn_name)}\b", source)
    if not m:
        return ""
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
    names: list[str] = []
    for m in re.finditer(r"(?m)^\s{4}([A-Z][A-Za-z0-9_]*)\s*(?:\{|,)", cli_source):
        names.append(camel_to_kebab(m.group(1)))
    return sorted(set(names))


def collect_cli_surface(repo_root: pathlib.Path, ref: str) -> dict[str, Any] | None:
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


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"differential-parity-gate-{stamp}.json"


def git_ref_exists(repo_root: pathlib.Path, ref: str) -> bool:
    proc = run(["git", "rev-parse", "--verify", ref], repo_root)
    return proc.returncode == 0


def ahead_behind(repo_root: pathlib.Path, local_ref: str, upstream_ref: str) -> tuple[int, int]:
    proc = run(
        ["git", "rev-list", "--left-right", "--count", f"{local_ref}...{upstream_ref}"],
        repo_root,
    )
    if proc.returncode != 0:
        return (0, 0)
    parts = proc.stdout.strip().split()
    if len(parts) != 2:
        return (0, 0)
    try:
        return (int(parts[0]), int(parts[1]))
    except ValueError:
        return (0, 0)


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    report_path = (
        pathlib.Path(args.report_path).expanduser().resolve()
        if args.report_path
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)

    local_exists = git_ref_exists(repo_root, args.local_ref)
    upstream_exists = git_ref_exists(repo_root, args.upstream_ref)
    if not local_exists or not upstream_exists:
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "ok": False,
            "reason": "missing_git_ref",
            "local_ref": args.local_ref,
            "upstream_ref": args.upstream_ref,
            "local_ref_exists": local_exists,
            "upstream_ref_exists": upstream_exists,
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            print("[differential-parity] FAILED (missing ref)")
            print(f"[differential-parity] Report: {report_path}")
        return 1

    local_surface = collect_cli_surface(repo_root, args.local_ref)
    upstream_surface = collect_cli_surface(repo_root, args.upstream_ref)
    if local_surface is None or upstream_surface is None:
        fallback = run_shell(args.fallback_cmd, repo_root)
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "ok": bool(fallback.get("ok")),
            "reason": "cli_surface_unavailable_fallback",
            "local_ref": args.local_ref,
            "upstream_ref": args.upstream_ref,
            "fallback": fallback,
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        report["report_path"] = str(report_path)
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            status = "PASSED" if report["ok"] else "FAILED"
            print(f"[differential-parity] {status} (fallback)")
            print(f"[differential-parity] Report: {report_path}")
        return 0 if report["ok"] else 1

    drift = compute_cli_surface_drift(local_surface, upstream_surface)
    ahead, behind = ahead_behind(repo_root, args.local_ref, args.upstream_ref)
    commits_gate_ok = behind <= max(0, args.max_commits_behind)
    gate_ok = bool(not drift.get("has_drift") and commits_gate_ok)

    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "ok": gate_ok,
        "local_ref": args.local_ref,
        "upstream_ref": args.upstream_ref,
        "commits": {
            "ahead": ahead,
            "behind": behind,
            "max_commits_behind": args.max_commits_behind,
            "behind_gate_ok": commits_gate_ok,
        },
        "drift": drift,
        "local_surface": local_surface,
        "upstream_surface": upstream_surface,
        "summary": {
            "missing_top_level": len(drift.get("top_level", {}).get("missing_in_local", [])),
            "missing_actions": int(drift.get("missing_action_count", 0)),
            "extra_top_level": len(drift.get("top_level", {}).get("extra_in_local", [])),
        },
    }

    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if report["ok"] else "FAILED"
        print(
            "[differential-parity] "
            f"{status} (missing_top={report['summary']['missing_top_level']} "
            f"missing_actions={report['summary']['missing_actions']} behind={behind})"
        )
        print(f"[differential-parity] Report: {report_path}")

    return 0 if gate_ok else 1


if __name__ == "__main__":
    sys.exit(main())
