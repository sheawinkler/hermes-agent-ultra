# Hermes Agent Ultra Differentiation Sequence Plan (2026-04-27)

Repository: `sheawinkler/hermes-agent-ultra`  
Objective: deliver six production differentiators with tests and operational docs.

## Issue Sequence

1. P0 — Deterministic replay + incident-pack integrity  
   Issue: #76
2. P0 — Tool/action policy engine hardening + explainable enforcement  
   Issue: #77
3. P1 — Adaptive performance governor (latency + error aware)  
   Issue: #78
4. P1 — Autonomous parity updater: richer draft-PR metadata + risk labels  
   Issue: #79
5. P2 — Memory fusion scoring + confidence metadata  
   Issue: #80
6. P2 — Doctor++ self-heal actions + snapshot action log  
   Issue: #81

Epic tracker: #82

## Implementation Gates

- Replay artifacts carry sequence/hash-chain metadata and redaction coverage for key/value secrets.
- Policy engine supports deny patterns and policy-file ingestion; deny responses are structured JSON.
- Governor uses runtime degradation signals (rolling latency/error) in token/concurrency throttling.
- Upstream sync PR bodies contain parity queue summary and drift artifact references, with labels.
- Memory fusion emits deterministic score/confidence metadata and stable tie-breaking.
- `doctor --self-heal` performs safe local remediations and records action outcomes in snapshot output.

## Validation Targets

- `cargo test -p hermes-agent`
- `cargo test -p hermes-tools`
- `cargo test -p hermes-cli`
- `python3 -m py_compile scripts/upstream_webhook_sync.py`
- `bash -n scripts/sync-upstream.sh`

