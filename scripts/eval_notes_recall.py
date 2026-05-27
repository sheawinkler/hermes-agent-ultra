#!/usr/bin/env python3
"""Evaluate meeting notes quality (action item recall, F1) against a fixture.

Usage:
    python3 scripts/eval_notes_recall.py --fixture <fixture_dir> [--output <json>]

The fixture directory must contain:
    transcript.txt          — Ground-truth diarized transcript
    expected_notes.json     — Ground-truth structured notes

The script:
1. Calls `hermes meeting notes --transcript` to generate notes from transcript.txt
2. Computes action_item F1 via LLM-as-judge (per-item binary match)
3. Writes results to logs/benchmark-meeting.json

Dependencies:
    pip install openai
    OPENAI_API_KEY or MEETING_LLM_API_KEY env var required for LLM judge.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Note generation
# ---------------------------------------------------------------------------

def generate_notes_from_transcript(transcript_text: str) -> dict[str, Any]:
    """Call the meeting_notes pipeline directly via Python (no subprocess needed)."""
    try:
        from hermes_tools_binding import run_meeting_notes  # type: ignore
        return run_meeting_notes(transcript_text)
    except ImportError:
        pass

    # Fallback: use LLM directly with the same prompt as meeting_notes.rs
    return _llm_generate_notes(transcript_text)


def _llm_generate_notes(transcript_text: str) -> dict[str, Any]:
    import openai  # type: ignore

    api_key = os.environ.get("MEETING_LLM_API_KEY") or os.environ.get("OPENAI_API_KEY", "")
    base_url = os.environ.get("MEETING_LLM_BASE_URL") or os.environ.get("OPENAI_BASE_URL")
    model = os.environ.get("MEETING_LLM_MODEL", "gpt-4o-mini")

    client = openai.OpenAI(api_key=api_key, base_url=base_url)

    system = (
        "你是一名专业会议纪要助手。请对以下会议转录进行结构化分析。"
        "仅返回合法 JSON，字段：summary、key_decisions、action_items、risks、follow_ups。"
        "所有字段用中文。"
    )
    # Strip speaker labels for LLM
    clean = re.sub(r"\[Speaker \w+\]\s*", "", transcript_text)

    response = client.chat.completions.create(
        model=model,
        temperature=0.2,
        max_tokens=900,
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": clean},
        ],
    )
    raw = response.choices[0].message.content or ""
    raw = raw.strip().lstrip("```json").lstrip("```").rstrip("```").strip()
    return json.loads(raw)


# ---------------------------------------------------------------------------
# LLM-as-judge: per-item binary match
# ---------------------------------------------------------------------------

def llm_judge_items(
    generated: list[str],
    expected: list[str],
    item_type: str = "action_item",
) -> dict[str, Any]:
    """Use LLM to judge whether each expected item is covered in the generated list."""
    if not expected:
        return {"precision": None, "recall": None, "f1": None, "note": "no expected items"}

    try:
        import openai  # type: ignore

        api_key = os.environ.get("MEETING_LLM_API_KEY") or os.environ.get("OPENAI_API_KEY", "")
        base_url = os.environ.get("MEETING_LLM_BASE_URL") or os.environ.get("OPENAI_BASE_URL")
        model = os.environ.get("MEETING_LLM_MODEL", "gpt-4o-mini")
        client = openai.OpenAI(api_key=api_key, base_url=base_url)
    except ImportError:
        return {"precision": None, "recall": None, "f1": None, "note": "openai not installed"}

    # Judge recall: for each expected item, is it covered by any generated item?
    recall_hits = 0
    recall_details = []
    for exp in expected:
        prompt = (
            f"Generated {item_type}s:\n"
            + "\n".join(f"- {g}" for g in generated)
            + f"\n\nExpected {item_type}: {exp}\n\n"
            "Is the expected item semantically covered by any of the generated items? "
            "Reply with a single JSON: {\"covered\": true} or {\"covered\": false}."
        )
        try:
            resp = client.chat.completions.create(
                model=model,
                temperature=0.0,
                max_tokens=20,
                messages=[{"role": "user", "content": prompt}],
            )
            raw = resp.choices[0].message.content or ""
            raw = raw.strip().lstrip("```json").lstrip("```").rstrip("```").strip()
            covered = json.loads(raw).get("covered", False)
        except Exception:
            covered = False
        recall_details.append({"expected": exp, "covered": covered})
        if covered:
            recall_hits += 1

    recall = recall_hits / len(expected)

    # Judge precision: for each generated item, is it relevant (not hallucinated)?
    precision_hits = 0
    for gen in generated:
        prompt = (
            f"Expected {item_type}s:\n"
            + "\n".join(f"- {e}" for e in expected)
            + f"\n\nGenerated {item_type}: {gen}\n\n"
            "Is the generated item grounded in (semantically related to) the expected items? "
            "Reply with a single JSON: {\"grounded\": true} or {\"grounded\": false}."
        )
        try:
            resp = client.chat.completions.create(
                model=model,
                temperature=0.0,
                max_tokens=20,
                messages=[{"role": "user", "content": prompt}],
            )
            raw = resp.choices[0].message.content or ""
            raw = raw.strip().lstrip("```json").lstrip("```").rstrip("```").strip()
            grounded = json.loads(raw).get("grounded", False)
        except Exception:
            grounded = False
        if grounded:
            precision_hits += 1

    precision = precision_hits / max(len(generated), 1)
    f1 = 2 * precision * recall / max(precision + recall, 1e-9)

    return {
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "f1": round(f1, 4),
        "recall_details": recall_details,
        "expected_count": len(expected),
        "generated_count": len(generated),
    }


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Evaluate meeting notes recall against fixture.")
    parser.add_argument("--fixture", required=True, help="Path to fixture directory")
    parser.add_argument("--output", help="Output JSON path")
    parser.add_argument(
        "--skip-judge",
        action="store_true",
        help="Skip LLM judge and only report item counts (faster, no API cost)",
    )
    args = parser.parse_args()

    fixture_dir = Path(args.fixture)
    transcript_path = fixture_dir / "transcript.txt"
    expected_path = fixture_dir / "expected_notes.json"

    if not transcript_path.exists():
        print(f"[eval_notes_recall] transcript.txt not found in {fixture_dir}", file=sys.stderr)
        sys.exit(1)
    if not expected_path.exists():
        print(f"[eval_notes_recall] expected_notes.json not found in {fixture_dir}", file=sys.stderr)
        sys.exit(1)

    transcript_text = transcript_path.read_text(encoding="utf-8")
    expected = json.loads(expected_path.read_text(encoding="utf-8"))

    print(f"[eval_notes_recall] Generating notes from transcript …")
    t0 = time.time()
    generated = generate_notes_from_transcript(transcript_text)
    elapsed = round(time.time() - t0, 2)
    print(f"[eval_notes_recall] Generation done in {elapsed}s")

    results: dict[str, Any] = {
        "fixture": str(fixture_dir),
        "elapsed_s": elapsed,
        "generated": generated,
    }

    if not args.skip_judge:
        print("[eval_notes_recall] Running LLM judge for action_items …")
        ai_metrics = llm_judge_items(
            generated.get("action_items", []),
            expected.get("action_items", []),
            "action_item",
        )
        print(f"  action_items — recall={ai_metrics.get('recall')}, precision={ai_metrics.get('precision')}, F1={ai_metrics.get('f1')}")

        print("[eval_notes_recall] Running LLM judge for key_decisions …")
        kd_metrics = llm_judge_items(
            generated.get("key_decisions", []),
            expected.get("key_decisions", []),
            "key_decision",
        )
        print(f"  key_decisions — recall={kd_metrics.get('recall')}, precision={kd_metrics.get('precision')}, F1={kd_metrics.get('f1')}")

        results["action_items"] = ai_metrics
        results["key_decisions"] = kd_metrics
    else:
        print("[eval_notes_recall] Skipping LLM judge (--skip-judge)")
        results["action_items"] = {
            "generated_count": len(generated.get("action_items", [])),
            "expected_count": len(expected.get("action_items", [])),
        }

    # Output
    if args.output:
        out_path = Path(args.output)
    else:
        hermes_home = Path(
            os.environ.get("HERMES_HOME") or os.path.expanduser("~/.hermes-agent-ultra")
        )
        out_path = hermes_home / "logs" / "benchmark-meeting.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)

    existing: list[dict[str, Any]] = []
    if out_path.exists():
        try:
            existing = json.loads(out_path.read_text(encoding="utf-8"))
            if not isinstance(existing, list):
                existing = [existing]
        except Exception:
            existing = []
    existing.append(results)
    out_path.write_text(json.dumps(existing, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"\n[eval_notes_recall] Results written to: {out_path}")

    # Exit non-zero if action_item F1 < target
    f1 = results.get("action_items", {}).get("f1")
    if isinstance(f1, float) and f1 < 0.80:
        print(
            f"[eval_notes_recall] WARNING: action_item F1 {f1:.1%} below 80% target",
            file=sys.stderr,
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
