#!/usr/bin/env python3
from __future__ import annotations

import json
import subprocess
from pathlib import Path

OWNER = "sheawinkler"
REPO = "hermes-agent-ultra"
PROJECT_TITLE = "hermes-ultra-alpha"
PLAN = Path("plans/alpha/hermes-ultra-alpha-61-elements.json")


def run(args: list[str], check: bool = True) -> str:
    p = subprocess.run(args, text=True, capture_output=True)
    if check and p.returncode != 0:
        raise RuntimeError(f"command failed: {' '.join(args)}\nstdout={p.stdout}\nstderr={p.stderr}")
    return p.stdout.strip()


def ensure_label(name: str, color: str, desc: str) -> None:
    rc = subprocess.run([
        "gh", "label", "create", name,
        "--repo", f"{OWNER}/{REPO}",
        "--color", color,
        "--description", desc,
    ], text=True, capture_output=True)
    if rc.returncode != 0 and "already exists" not in (rc.stderr or "").lower():
        raise RuntimeError(rc.stderr)


def get_or_create_project() -> int:
    raw = run(["gh", "project", "list", "--owner", OWNER, "--format", "json"])
    rows = json.loads(raw)
    for r in rows.get("projects", []):
        if r.get("title") == PROJECT_TITLE:
            return int(r["number"])
    out = run(["gh", "project", "create", "--owner", OWNER, "--title", PROJECT_TITLE, "--format", "json"])
    row = json.loads(out)
    return int(row["number"])


def issue_for_id(alpha_id: str) -> dict | None:
    q = f'"[{alpha_id}]" in:title'
    out = run(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            f"{OWNER}/{REPO}",
            "--search",
            q,
            "--json",
            "number,title,url",
            "--limit",
            "1",
        ]
    )
    rows = json.loads(out)
    return rows[0] if rows else None


def create_issue(item: dict) -> dict:
    alpha_id = item["id"]
    area = item["area"]
    title = item["title"]
    priority = item["priority"]
    trading = bool(item.get("trading_sensitive", False))

    t = f"[{alpha_id}] {area}: {title}"
    branch_note = (
        "- Implementation branch policy: **LOCAL/PRIVATE ONLY** (do not merge trading logic to public main)."
        if trading
        else "- Implementation branch policy: public repo branch + PR to main."
    )
    body = f"""## Objective
Implement `{alpha_id}` in Hermes Ultra Alpha scope.

## Spec
- Area: `{area}`
- Priority: `{priority}`
- Trading-sensitive: `{'yes' if trading else 'no'}`
{branch_note}

## Acceptance Criteria
- [ ] End-to-end behavior implemented (no placeholders)
- [ ] Tests added/updated and passing
- [ ] TUI/UX interactive validation completed where relevant
- [ ] ContextLattice checkpoint + readback evidence captured
- [ ] Parity artifacts updated when behavior surface changes

## Notes
- Parent project: `{PROJECT_TITLE}`
- Element source: `plans/alpha/hermes-ultra-alpha-61-elements.json`
"""

    labels = ["alpha"]
    labels.append(f"priority:{priority.lower()}")
    if trading:
        labels.append("trading-private")

    cmd = [
        "gh", "issue", "create",
        "--repo", f"{OWNER}/{REPO}",
        "--title", t,
        "--body", body,
    ]
    for l in labels:
        cmd.extend(["--label", l])

    url = run(cmd)
    num = int(url.rstrip("/").split("/")[-1])
    return {"number": num, "title": t, "url": url}


def add_to_project(project_number: int, issue_url: str) -> None:
    subprocess.run([
        "gh", "project", "item-add", str(project_number),
        "--owner", OWNER,
        "--url", issue_url,
    ], text=True, capture_output=True)


def main() -> None:
    ensure_label("alpha", "5319e7", "Hermes Ultra Alpha project scope")
    ensure_label("trading-private", "b60205", "Trading-sensitive item; keep implementation local/private")
    ensure_label("priority:p0", "d93f0b", "Highest priority")
    ensure_label("priority:p1", "fbca04", "High priority")

    project_number = get_or_create_project()
    print(f"project_number={project_number}")

    items = json.loads(PLAN.read_text())
    created = 0
    reused = 0
    for item in items:
        existing = issue_for_id(item["id"])
        if existing:
            issue = existing
            reused += 1
        else:
            issue = create_issue(item)
            created += 1
        add_to_project(project_number, issue["url"])
    print(f"created={created} reused={reused} total={len(items)}")


if __name__ == "__main__":
    main()
