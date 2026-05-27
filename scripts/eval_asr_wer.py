#!/usr/bin/env python3
"""Evaluate ASR accuracy (WER/CER) against a fixture transcript.

Usage:
    python3 scripts/eval_asr_wer.py --fixture <fixture_dir> [--provider openai] [--output <json>]

The fixture directory must contain:
    audio.wav          — 16kHz mono WAV (download separately, see fixtures/meeting_notes/README.md)
    transcript.txt     — Ground-truth transcript (speaker-labeled lines)

Output JSON is written to .hermes-agent-ultra/logs/benchmark-asr.json by default.

Dependencies:
    pip install jiwer openai  (or set STT_PROVIDER=groq and GROQ_API_KEY)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# WER / CER computation
# ---------------------------------------------------------------------------

def compute_wer(reference: str, hypothesis: str) -> dict[str, float]:
    """Compute WER and CER using jiwer if available, else a simple baseline."""
    try:
        import jiwer  # type: ignore

        measures = jiwer.compute_measures(reference, hypothesis)
        cer_tr = jiwer.Compose([jiwer.RemoveMultipleSpaces(), jiwer.Strip()])
        cer_ref = " ".join(list(cer_tr(reference)))
        cer_hyp = " ".join(list(cer_tr(hypothesis)))
        cer = jiwer.compute_measures(cer_ref, cer_hyp)["wer"]
        return {
            "wer": round(measures["wer"], 4),
            "cer": round(cer, 4),
            "insertions": measures["insertions"],
            "deletions": measures["deletions"],
            "substitutions": measures["substitutions"],
        }
    except ImportError:
        # Fallback: token-level edit distance
        ref_tokens = reference.split()
        hyp_tokens = hypothesis.split()
        return {"wer": _simple_wer(ref_tokens, hyp_tokens), "cer": None, "note": "jiwer not installed — basic WER"}


def _simple_wer(ref: list[str], hyp: list[str]) -> float:
    """Levenshtein word error rate."""
    n, m = len(ref), len(hyp)
    dp = list(range(m + 1))
    for i in range(1, n + 1):
        new_dp = [i] + [0] * m
        for j in range(1, m + 1):
            if ref[i - 1] == hyp[j - 1]:
                new_dp[j] = dp[j - 1]
            else:
                new_dp[j] = 1 + min(dp[j], new_dp[j - 1], dp[j - 1])
        dp = new_dp
    return round(dp[m] / max(n, 1), 4)


# ---------------------------------------------------------------------------
# STT
# ---------------------------------------------------------------------------

def transcribe_audio(audio_path: str, provider: str = "openai") -> str:
    """Call the configured STT provider and return transcript text."""
    if provider == "openai":
        import openai  # type: ignore
        client = openai.OpenAI()
        with open(audio_path, "rb") as f:
            result = client.audio.transcriptions.create(model="whisper-1", file=f)
        return result.text
    elif provider == "groq":
        from groq import Groq  # type: ignore
        client = Groq()
        with open(audio_path, "rb") as f:
            result = client.audio.transcriptions.create(model="whisper-large-v3-turbo", file=f)
        return result.text
    elif provider == "hermes":
        # Use hermes CLI as STT backend
        result = subprocess.run(
            ["hermes", "--oneshot", f"transcribe the audio file at {audio_path}"],
            capture_output=True, text=True, timeout=120,
        )
        return result.stdout.strip()
    else:
        raise ValueError(f"Unknown STT provider: {provider}")


# ---------------------------------------------------------------------------
# Ground-truth loading
# ---------------------------------------------------------------------------

def load_ground_truth(fixture_dir: Path) -> str:
    """Load transcript.txt, stripping speaker labels."""
    txt = (fixture_dir / "transcript.txt").read_text(encoding="utf-8")
    # Remove "[Speaker X] " prefixes
    lines = [re.sub(r"^\[Speaker \w+\]\s*", "", line).strip() for line in txt.splitlines()]
    return " ".join(l for l in lines if l)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Evaluate ASR WER/CER against fixture.")
    parser.add_argument("--fixture", required=True, help="Path to fixture directory")
    parser.add_argument("--provider", default="openai", help="STT provider (openai, groq, hermes)")
    parser.add_argument("--output", help="Output JSON path (default: logs/benchmark-asr.json)")
    args = parser.parse_args()

    fixture_dir = Path(args.fixture)
    audio_path = fixture_dir / "audio.wav"

    if not audio_path.exists():
        print(
            f"[eval_asr_wer] audio.wav not found in {fixture_dir}.\n"
            "Download the audio file (see fixtures/meeting_notes/README.md) and retry.",
            file=sys.stderr,
        )
        sys.exit(1)

    print(f"[eval_asr_wer] Transcribing {audio_path} with provider={args.provider} …")
    t0 = time.time()
    hypothesis = transcribe_audio(str(audio_path), provider=args.provider)
    elapsed = round(time.time() - t0, 2)
    print(f"[eval_asr_wer] Transcription done in {elapsed}s")

    reference = load_ground_truth(fixture_dir)
    metrics = compute_wer(reference, hypothesis)
    metrics["elapsed_s"] = elapsed
    metrics["fixture"] = str(fixture_dir)
    metrics["provider"] = args.provider

    # Determine output path
    if args.output:
        out_path = Path(args.output)
    else:
        hermes_home = Path(
            os.environ.get("HERMES_HOME") or os.path.expanduser("~/.hermes-agent-ultra")
        )
        out_path = hermes_home / "logs" / "benchmark-asr.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)

    # Accumulate results (append to existing log)
    existing: list[dict[str, Any]] = []
    if out_path.exists():
        try:
            existing = json.loads(out_path.read_text(encoding="utf-8"))
            if not isinstance(existing, list):
                existing = [existing]
        except Exception:
            existing = []
    existing.append(metrics)
    out_path.write_text(json.dumps(existing, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"\n[eval_asr_wer] Results:")
    print(f"  WER : {metrics.get('wer', 'N/A')}")
    print(f"  CER : {metrics.get('cer', 'N/A')}")
    print(f"  Written to: {out_path}")

    # Exit non-zero if WER > target (8% for Mandarin, 15% for English)
    wer = metrics.get("wer")
    if isinstance(wer, float) and wer > 0.15:
        print(f"[eval_asr_wer] WARNING: WER {wer:.1%} exceeds 15% threshold", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
