# Cargo Build Surface Cratification

Date: 2026-06-22

## Local Evidence

- A focused `hermes-cli` OpenAI OAuth routing test took `4m01s` to compile before running one test.
- A focused behavioral parity gate took `2m51s` and compiled `hermes-cli` because the parity crate has a dev dependency on the full CLI crate.
- `hermes-cli` owns three binaries: `hermes`, `hermes-ultra`, and `hermes-agent-ultra`.
- `hermes-cli` directly depends on runtime, gateway, cron, ACP, MCP, tools, skills, auth, intelligence, telemetry, TUI, clipboard, archive, and installer dependencies.
- `hermes-cli` currently enables the full gateway adapter feature set, so small provider/auth changes can invalidate a broad adapter/runtime build surface.

## Target Shape

Keep the runtime Rust-only, but split compile surfaces so targeted work can test targeted crates:

1. `hermes-provider-runtime`
   - Own `build_provider`, provider/model selection, OpenAI ChatGPT OAuth routing, local OpenAI-compatible backend routing, and provider auth repair contracts.
   - No TUI, installer, adapter, cron, or gateway feature dependencies.

2. `hermes-app-runtime`
   - Own noninteractive chat orchestration, prompt reformulation, tool-planning handoff, memory/context policy injection, and agent-loop wiring.
   - Depend on provider runtime and core agent crates, not on CLI wrappers or TUI.

3. `hermes-cli-ui`
   - Own terminal UI, clipboard, slash-command rendering, and completion/help presentation.
   - Keep UI dependencies out of provider/auth tests.

4. Gateway adapter feature narrowing
   - Move broad adapter feature enablement behind explicit binary/runtime feature sets.
   - Keep provider/auth and command-contract tests from compiling every gateway adapter by default.

5. Parity test dependency narrowing
   - Point behavioral/provider parity tests at `hermes-provider-runtime` and command-contract crates where possible.
   - Keep full `hermes-cli` parity checks for end-to-end CLI behavior only.

## Gates

- `scripts/audit-cargo-build-surface.sh`
- `cargo test -p hermes-provider-runtime`
- `cargo test -p hermes-app-runtime`
- `cargo test -p hermes-parity-tests test_behavioral_similarity_diff_gate_passes_and_references_real_tests -- --nocapture`
- `cargo build -p hermes-cli --bin hermes-ultra --bin hermes-agent-ultra`

## Non-Goals

- Do not split crates just to reduce a dependency count if it makes runtime behavior harder to audit.
- Do not change public CLI behavior during the build-surface split.
- Do not move gateway adapter behavior out of Rust or behind Python fallback.
