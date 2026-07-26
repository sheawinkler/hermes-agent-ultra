# Issue #702 production-completeness audit

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
Verdict: `0 complete / 12 partial / 3 facade-only` under the production callsite + real consumer + failure test + public smoke + eval-ledger contract.

## Facade-only capabilities

- Domain-model generator: static recommended-skill/catalog entry; no producer or consumer.
- Architecture report: static skill/catalog entry; `/graph` diagnostics are not an architecture report.
- Grill-me mode: static skill entries; generic `clarify` is not a grilling workflow.

## Partial capabilities to close without rebuilding

- Skills: reuse tap merge, install path, and `SkillGuard`.
- OIDC: reuse real dashboard routes/guard; add protocol lifecycle integration evidence.
- Proof cockpit: reuse verification/telemetry readers; derive from typed runtime evidence.
- Replay and recorder: reuse `ReplayRecorder` and replay/raw-trace consumers.
- Skill scanner: reuse `SkillGuard`; remove/route around the duplicate CLI regex scanner.
- Smart routing: production path is `hermes_agent::smart_model_routing::resolve_turn_route`; `hermes_intelligence::SmartModelRouter` is a duplicate unless deliberately integrated.
- Objectives: reuse the alpha objective contract and snapshot plane.
- Chaos: reuse real adapter chaos fixtures and policy simulation; replace the cockpit checklist with those producers.
- Onboarding: compose existing boot, walkthrough, project facts, and project tree into a real journey.
- Team memory: reuse the memory manager and ContextLattice adapter; add scope/privacy/concurrency semantics.
- Delegation: reuse `SubAgentOrchestrator`; do not call it a marketplace without admission/matching economics.

## Required order

P0 integrity first; P1 typed transactional evidence/outbox; P2 sandbox/package trust/privacy; P3 replay and real evals; P4 promotion gate. Only then promote the partial capabilities or implement the three facades. Every closure packet must update this handoff and the canonical roadmap with direct evidence.

## Cockpit truth cutover

`harness_cockpit` now reports a typed, machine-readable maturity summary derived from the item inventory: `0 complete / 12 partial / 3 facade-only`. Every item exposes observed evidence separately from the missing production/smoke/evaluation contract. Skills are labeled recommendations, proof is labeled diagnostic, and chaos scenarios are labeled non-executable checklists; catalog presence can no longer serialize as an implemented capability. Six focused cockpit regressions passed.

## Adjacent voice/media truth cutover

The former Rust `voice_mode`/voice-mixer facades and built-in skill/catalog claims were removed rather than counted as an interactive voice product. `/voice` now reports only that no separate interactive engine is active. Public setup, messaging, reference, and bundled Hermes-skill documentation no longer claims microphone capture, automatic spoken replies, live Discord voice channels, automatic inbound transcription, local Whisper, xAI STT, or inbound QQ behavior that the Rust runtime does not implement.

The retained production surfaces are real dispatch paths:

- `text_to_speech`: OpenAI, ElevenLabs, MiniMax, Piper, and Unix command providers; bounded HTTP/process execution; structural container validation; atomic destination replacement; current-turn media delivery.
- `transcription`: an explicit OpenAI/Nous-managed, Groq, or Mistral HTTP tool and the Teams recording consumer. External upload is limited to canonical regular files under configured media roots and read through the same validated, no-symlink file handle; arbitrary readable paths, root-ancestor swaps, and symlink escapes fail before upload.
- QQ: outbound text/markdown/image delivery only. The adapter does not advertise an inbound WebSocket/STT product.

The five held media paths are now closed in focused regression coverage: MiniMax defaults and provider output validation; consumer-aware Teams extraction limits; timeout/cancellation cleanup for ffmpeg descendants; owner-only atomic TTS replacement; and CR/LF-safe deliverable destinations. Additional closure rejects hard-linked upload sources, provider-oversize STT before allocation/network, unsafe ACL/DACL roots, HTTP redirects, non-finite duration inputs, and non-Opus “voice-compatible” output.

Observed focused gates: TTS 118 passed; transcription 27 passed; media policy 22 passed; gateway media delivery 9 default + 2 staged-route + 9 Discord-feature passed; Rust Teams 26 passed plus 4 CLI contract tests; integrated `hermes-cli` library check passed. Windows ACL/Job Object behavior remains unexecuted on this Darwin host, and no live Graph/STT/TTS credentials were used, so those platform/live checks remain explicit release evidence gaps rather than inferred passes.

Teams truth is split by shipped surface. The Rust release implements a manual, organizer-scoped Graph fetch/run/replay contract with bounded artifacts and WebVTT normalization; it rejects webhook/sink configuration and does not advertise automatic delivery. The Python Teams plugin source was hardened and its focused suite passes, but Docker/release artifacts do not package that runtime, so public docs and skills label it optional source/reference only. Automatic Graph trigger, durable background retry, and post-back to Teams remain open until the authoritative P1 ledger/outbox and P5 automation path are implemented; no best-effort duplicate scheduler is accepted.

These changes close false voice/media advertising and the recorded five-path media gate; they do not reclassify any of the 15 issue-#702 capabilities above.

## Remaining risks

- `harness_cockpit` is an evidence index, not a subsystem owner.
- Static catalog rows and prose outcome scores must not be used as completion evidence.
- The duplicate skill scanner and duplicate router can produce contradictory truth until consolidated.
