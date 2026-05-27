# Meeting Notes Benchmark Fixtures

Each subdirectory contains one benchmark case.

## Directory structure

```
<case_id>/
  transcript.txt          — Ground-truth diarized transcript (human-verified)
                            Format: "[Speaker A] text\n[Speaker B] text\n…"
  expected_notes.json     — Ground-truth structured notes
  audio.wav               — (Optional) Audio file for ASR evaluation
                            Not committed to git; download separately (see below)
```

## `expected_notes.json` schema

```json
{
  "summary": "string (≤400 chars)",
  "key_decisions": ["string", "…"],
  "action_items":  ["string", "…"],
  "risks":         ["string", "…"],
  "follow_ups":    ["string", "…"]
}
```

## Audio sources

| Case | Dataset | License | Download |
|------|---------|---------|----------|
| `zh_2speaker_sample` | AISHELL-4 | Apache 2.0 | https://www.openslr.org/111/ |
| `en_2speaker_sample` | AMI Corpus | CC BY 4.0 | https://groups.inf.ed.ac.uk/ami/download/ |

After downloading, place `audio.wav` (16kHz mono WAV) in the corresponding fixture directory.

## Running benchmarks

```bash
# ASR accuracy (requires audio.wav)
python3 scripts/eval_asr_wer.py --fixture crates/hermes-parity-tests/fixtures/meeting_notes/zh_2speaker_sample

# Notes recall (uses transcript.txt, no audio needed)
python3 scripts/eval_notes_recall.py --fixture crates/hermes-parity-tests/fixtures/meeting_notes/zh_2speaker_sample

# Memory integration test
cargo test -p hermes-parity-tests -- meeting_memory
```
