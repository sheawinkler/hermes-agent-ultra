#!/usr/bin/env python3
"""Generate upstream test-intent to Rust evidence mapping artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class IntentSpec:
    id: str
    upstream_scope: list[str]
    local_evidence_globs: list[str]


INTENTS: list[IntentSpec] = [
    IntentSpec(
        id="gateway-platform-behavior",
        upstream_scope=["tests/gateway", "gateway/platforms"],
        local_evidence_globs=[
            "crates/hermes-gateway/src/platforms/*.rs",
            "crates/hermes-cli/tests/e2e_gateway_*.rs",
        ],
    ),
    IntentSpec(
        id="tool-runtime-behavior",
        upstream_scope=["tests/tools", "tools/environments"],
        local_evidence_globs=[
            "crates/hermes-tools/src/**/*.rs",
            "crates/hermes-tools/tests/*.rs",
        ],
    ),
    IntentSpec(
        id="cli-command-surface",
        upstream_scope=["tests/hermes_cli", "tests/cli"],
        local_evidence_globs=[
            "crates/hermes-cli/src/**/*.rs",
            "crates/hermes-parity-tests/tests/cli_command_contract.rs",
        ],
    ),
    IntentSpec(
        id="agent-loop-and-runtime",
        upstream_scope=["tests/run_agent", "tests/agent"],
        local_evidence_globs=[
            "crates/hermes-agent/src/**/*.rs",
            "crates/hermes-agent/tests/*.rs",
        ],
    ),
    IntentSpec(
        id="acp-protocol-and-transport",
        upstream_scope=["tests/acp"],
        local_evidence_globs=[
            "crates/hermes-acp/src/**/*.rs",
            "crates/hermes-acp/tests/*.rs",
        ],
    ),
    IntentSpec(
        id="skills-management-contract",
        upstream_scope=["tests/skills", "skills", "optional-skills"],
        local_evidence_globs=[
            "crates/hermes-skills/src/**/*.rs",
            "crates/hermes-cli/src/commands.rs",
        ],
    ),
    IntentSpec(
        id="cron-and-scheduler-runtime",
        upstream_scope=["tests/cron"],
        local_evidence_globs=[
            "crates/hermes-cron/src/**/*.rs",
            "crates/hermes-cli/src/commands.rs",
        ],
    ),
    IntentSpec(
        id="memory-plugin-integration",
        upstream_scope=["plugins/memory", "tests/plugins"],
        local_evidence_globs=[
            "crates/hermes-agent/src/memory_plugins/*.rs",
            "crates/hermes-agent/tests/parity_self_evolution_fixtures.rs",
        ],
    ),
    IntentSpec(
        id="environment-lifecycle-contract",
        upstream_scope=["environments/benchmarks", "tools/environments"],
        local_evidence_globs=[
            "crates/hermes-environments/src/**/*.rs",
            "crates/hermes-environments/tests/*.rs",
        ],
    ),
    IntentSpec(
        id="tool-call-parser-contract",
        upstream_scope=["environments/tool_call_parsers", "tests/integration"],
        local_evidence_globs=[
            "crates/hermes-core/src/tool_call_parser.rs",
            "crates/hermes-parity-tests/fixtures/hermes_core/*.json",
        ],
    ),
]


def collect_matches(repo_root: Path, patterns: list[str]) -> list[str]:
    matched: set[str] = set()
    for pattern in patterns:
        for p in repo_root.glob(pattern):
            if p.is_file():
                matched.add(p.relative_to(repo_root).as_posix())
    return sorted(matched)


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate test-intent mapping report.")
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument(
        "--out-json",
        default="docs/parity/test-intent-mapping.json",
        type=Path,
        help="Output JSON path (relative to repo root)",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/test-intent-mapping.md",
        type=Path,
        help="Output Markdown path (relative to repo root)",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    rows = []
    mapped = 0
    for intent in INTENTS:
        evidence = collect_matches(repo_root, intent.local_evidence_globs)
        is_mapped = len(evidence) > 0
        if is_mapped:
            mapped += 1
        rows.append(
            {
                "id": intent.id,
                "upstream_scope": intent.upstream_scope,
                "mapped": is_mapped,
                "evidence_count": len(evidence),
                "evidence_sample": evidence[:25],
            }
        )

    ratio = mapped / len(INTENTS) if INTENTS else 0.0
    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "intent_unit": "domain_intent",
        "summary": {
            "total_intents": len(INTENTS),
            "mapped_intents": mapped,
            "unmapped_intents": len(INTENTS) - mapped,
            "mapping_ratio": round(ratio, 4),
        },
        "intents": rows,
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md = [
        "# Test Intent Mapping",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "| Intent | Mapped | Evidence Count |",
        "| --- | --- | ---: |",
    ]
    for row in rows:
        md.append(
            f"| `{row['id']}` | {'yes' if row['mapped'] else 'no'} | {row['evidence_count']} |"
        )
    md.append("")
    md.append(
        f"- Mapping ratio: `{payload['summary']['mapped_intents']}` / "
        f"`{payload['summary']['total_intents']}` = "
        f"`{payload['summary']['mapping_ratio']}`."
    )
    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")

    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

