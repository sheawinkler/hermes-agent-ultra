#!/usr/bin/env python3
"""Generate parity proof focused on GPAR-01..GPAR-04 queue closure."""

from __future__ import annotations

import datetime as dt
import json
from collections import Counter, defaultdict
from pathlib import Path

TARGET_TICKETS = [20, 21, 22, 23]
TICKET_NAME = {
    20: "GPAR-01 tests+CI parity",
    21: "GPAR-02 skills parity",
    22: "GPAR-03 UX parity",
    23: "GPAR-04 gateway/plugin-memory parity",
}


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    queue_path = repo_root / "docs/parity/upstream-missing-queue.json"
    out_json = repo_root / "docs/parity/gpar-01-04-proof.json"
    out_md = repo_root / "docs/parity/gpar-01-04-proof.md"

    queue = json.loads(queue_path.read_text(encoding="utf-8"))
    rows = [r for r in queue["commits"] if int(r.get("target_ticket", 0)) in TARGET_TICKETS]

    by_ticket_disposition: dict[int, Counter[str]] = defaultdict(Counter)
    by_ticket_total: Counter[int] = Counter()
    by_ticket_pending: Counter[int] = Counter()
    by_disposition: Counter[str] = Counter()

    for row in rows:
        ticket = int(row["target_ticket"])
        disposition = str(row.get("disposition", "pending"))
        by_ticket_total[ticket] += 1
        by_ticket_disposition[ticket][disposition] += 1
        by_disposition[disposition] += 1
        if disposition == "pending":
            by_ticket_pending[ticket] += 1

    total_pending = sum(by_ticket_pending.values())
    passed = total_pending == 0

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "scope": {
            "tickets": TARGET_TICKETS,
            "labels": {str(k): TICKET_NAME[k] for k in TARGET_TICKETS},
        },
        "summary": {
            "total_commits": len(rows),
            "by_disposition": dict(sorted(by_disposition.items())),
            "total_pending": total_pending,
            "pass": passed,
        },
        "ticket_breakdown": {
            str(ticket): {
                "label": TICKET_NAME[ticket],
                "total": by_ticket_total[ticket],
                "pending": by_ticket_pending[ticket],
                "by_disposition": dict(sorted(by_ticket_disposition[ticket].items())),
            }
            for ticket in TARGET_TICKETS
        },
    }

    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md: list[str] = []
    md.append("# GPAR-01..04 Parity Proof")
    md.append("")
    md.append(f"Generated: `{payload['generated_at_utc']}`")
    md.append("")
    md.append(f"- Scope tickets: `{', '.join(str(t) for t in TARGET_TICKETS)}`")
    md.append(f"- Total scoped commits: `{len(rows)}`")
    md.append(f"- Pending scoped commits: `{total_pending}`")
    md.append(f"- Gate: `{'PASS' if passed else 'FAIL'}`")
    md.append("")
    md.append("| Ticket | Label | Total | Pending | Ported | Superseded |")
    md.append("| ---: | --- | ---: | ---: | ---: | ---: |")
    for ticket in TARGET_TICKETS:
        disp = by_ticket_disposition[ticket]
        md.append(
            f"| #{ticket} | {TICKET_NAME[ticket]} | {by_ticket_total[ticket]} | "
            f"{by_ticket_pending[ticket]} | {disp.get('ported', 0)} | {disp.get('superseded', 0)} |"
        )
    md.append("")

    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
