#!/usr/bin/env python3
"""Record equity research golden fixtures from UZI fin_models.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

UZI_SCRIPTS = Path(r"c:\code\github\UZI-Skill\skills\deep-analysis\scripts")
OUT = Path(__file__).resolve().parents[1] / "crates/hermes-parity-tests/fixtures/trading_research/models_golden.json"


def main() -> int:
    sys.path.insert(0, str(UZI_SCRIPTS))
    from lib.fin_models import (  # noqa: PLC0415
        build_comps_table,
        compute_dcf,
        compute_wacc,
        project_three_stmt,
        quick_lbo,
    )

    smoke = {
        "price": 18.5,
        "market_cap_yi": 260,
        "shares_outstanding_yi": 14.0,
        "revenue_latest_yi": 52,
        "net_margin": 12.5,
        "pe": 35,
        "pb": 2.8,
        "total_debt_yi": 10,
        "cash_yi": 40,
        "fcf_latest_yi": 6.5,
        "ebitda_yi": 10,
        "equity_yi": 92,
    }

    dcf = compute_dcf(smoke)
    peers = [
        {"name": "P1", "pe": 28, "pb": 2.1, "ps": 3, "roe": 18, "net_margin": 14, "revenue_growth": 12},
        {"name": "P2", "pe": 32, "pb": 2.5, "ps": 3.5, "roe": 16, "net_margin": 12, "revenue_growth": 10},
    ]
    target = {"price": 18.5, "pe": 35, "pb": 2.8, "eps": 0.53, "bvps": 6.6}
    comps = build_comps_table(target, peers)
    lbo = quick_lbo(smoke)
    stmt = project_three_stmt(smoke)

    fixture = {
        "schema_version": 1,
        "fixture_group": "trading_research",
        "cases": [
            {
                "id": "wacc_default",
                "op": "compute_wacc",
                "input": {},
                "expected": {"wacc": compute_wacc()["wacc"]},
            },
            {
                "id": "dcf_smoke",
                "op": "compute_dcf",
                "input": smoke,
                "expected": {
                    "intrinsic_per_share": dcf["intrinsic_per_share"],
                    "safety_margin_pct": dcf["safety_margin_pct"],
                    "center_cell": dcf["sensitivity_table"]["center_cell"],
                    "base_fcf_yi": dcf["base_fcf_yi"],
                },
            },
            {
                "id": "comps_peers",
                "op": "build_comps",
                "input": {"target": target, "peers": peers},
                "expected": {
                    "median_pe": comps["peer_stats"]["pe"]["median"],
                    "implied_pe": comps["implied_price"].get("via_median_pe"),
                },
            },
            {
                "id": "lbo_smoke",
                "op": "quick_lbo",
                "input": smoke,
                "expected": {"irr_pct": lbo["irr_pct"], "moic": lbo["moic"]},
            },
            {
                "id": "three_stmt_smoke",
                "op": "project_three_stmt",
                "input": smoke,
                "expected": {"y5_ni": stmt["income_statement"]["net_income"][-1]},
            },
        ],
    }

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
