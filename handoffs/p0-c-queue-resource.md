# P0-C: queue, deduplication, and resource integrity

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
Pre-edit verdict: confirmed production defects, not duplicate work. The prior busy queue reused sender provenance and could remove pending work before propagating current-turn failure/cancellation. Resource ceilings were not consistently enforced across sessions, background tasks, and HTTP WebSockets.

## Intentional changes

- Transactional lease-owned busy queue with reset/error/abort recovery.
- Sender-provenance envelopes retained per queued message.
- Exact 33-ID drain and deduplication coverage in `crates/hermes-gateway/src/gateway/tests/queue_dedup.rs`.
- Ingress deduplication claims after pure policy rejects but before DM state mutation or sends.
- Successful Pair/Deny paths mark deduplication complete; failed sends remain retryable.
- Resource ceilings cover sessions, background tasks, and HTTP WebSockets.
- Compatibility gate: `GatewayConfig.resource_limits_enabled` defaults to `false`.
- Configuration property initializer repaired.

## Files touched

Primary surfaces are under:

- `crates/hermes-gateway/src/gateway/`
- `crates/hermes-gateway/src/session_control.rs`
- `crates/hermes-gateway/src/session.rs`
- `crates/hermes-gateway/src/background.rs`
- `crates/hermes-http/src/lib.rs`
- `crates/hermes-config/src/`
- corresponding focused tests

See the live working diff for the exact integration set; this packet shares files with other P0 work.

## Verification completed

Post-P0-B integration, Main ran the nonincremental focused package suite:

- `cargo test -p hermes-gateway --lib -- --nocapture`: 395 passed, 0 failed, including 33-ID FIFO drain, sender provenance, retry/reset/panic/cancellation, deduplication, scoped authorization, session/background capacity, and failure rollback.
- `cargo test -p hermes-http --lib -- --nocapture`: 17 passed, 0 failed, including opt-in WebSocket buffer limits.
- `cargo test -p hermes-config --lib -- --nocapture`: 204 passed, 0 failed, including opt-in resource-limit round-trip/display behavior.
- Aggregate: 616 passed, 0 failed in 94.33 seconds of measured wall time.

## Speech/media resource-integrity extension

The fake interactive voice surface was removed; retained speech/media behavior follows shipped tool and consumer callsites, but final security review has **withheld P0 closure**:

- TTS has bounded OpenAI, ElevenLabs, MiniMax, Piper, and Unix command paths with structural container validation and atomic replacement. Corrections are still required for MiniMax's default emotion contract, CR/LF destinations, and restrictive destination permissions/ACLs.
- STT supports explicit OpenAI/Nous-managed, Groq, and Mistral HTTP routes. External audio must resolve beneath a configured media capability root. Unix opens every root/path component from `/` with no-follow `openat`; Windows rejects reparse points and compares the final opened-handle path. The same opened handle supplies upload bytes.
- The Teams recording fallback uses a private per-run temp root, carries `stt_model`, and cleans direct files on observed success/failure/cancellation. It must still replace uncompressed WAV fallback with provider-bounded compressed chunks and terminate/wait the entire ffmpeg descendant tree with a bounded stderr join.
- Public setup, messaging, reference, bundled-skill, QQ, and voice documentation distinguishes these tools from unsupported microphone, spoken-reply, live voice-channel, automatic inbound-transcription, and QQ inbound/STT products.

The preceding focused regression run covered 81 TTS tests, 23 transcription/registry tests, three Teams production-transcriber lifecycle tests, two Teams ffmpeg outcome tests, 14 config media-policy tests, and 23 gateway media-policy/delivery tests: 146 passed, 0 failed. Those tests did not cover the five blockers above and therefore are not completion evidence. An exact Windows opened-handle probe type-checked against `windows-sys` for `x86_64-pc-windows-gnu`; the full package cross-check reached the native `ring` build and was blocked because this workstation lacks `x86_64-w64-mingw32-gcc`.

## Remaining risks

- Durable-grant integration is verified; rerun the focused gateway/CLI suite once the corrected Discord ingress packet lands.
- Resource limits remain opt-in until operator migration and production capacity baselines are available.
- Do not weaken lease ownership or deduplication ordering while resolving merge overlap.
- Final speech/media security review must pass after the five provider, chunking, process-tree, permissions, and destination-path corrections.
