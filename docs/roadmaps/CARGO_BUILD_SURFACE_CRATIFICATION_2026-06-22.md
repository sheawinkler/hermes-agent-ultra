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
   - Status: implemented as the first split.
   - Owns `build_provider`, provider/model selection, OpenAI ChatGPT OAuth routing, local OpenAI-compatible backend routing, and provider auth repair contracts.
   - No TUI, installer, adapter, cron, or gateway feature dependencies.

2. `hermes-app-runtime`
   - Status: third split implemented.
   - Owns agent configuration construction, query-mode provider/model/env/tool policy, model-catalog remediation selection, noninteractive query agent-loop wiring, assistant reply extraction, and runtime prompt reformulation policy.
   - Keeps provider construction injected by the CLI so OpenAI OAuth/auth-state routing remains in the existing runtime path.
   - Remaining memory/context policy injection is already runtime-owned where it is generic; UI-only ContextLattice status events remain in the CLI app.
   - Depend on provider runtime and core agent crates, not on CLI wrappers or TUI.

3. `hermes-cli-ui`
   - Status: first crate split implemented; tool-preview presentation split implemented.
   - Owns slash-command rendering, autocomplete ranking, alias canonicalization, completion/help presentation, tool emoji mapping, compact tool-call previews, and gateway tool-progress rendering.
   - Depends only on `serde_json` beyond the standard library so preview rendering can stay out of `hermes-cli` without pulling TUI/runtime crates into narrow UI tests.
   - Next: broader terminal UI, checklist, theme, and clipboard extraction should wait for a dedicated TUI crate boundary.
   - Keep UI dependencies out of provider/auth tests.

4. Gateway adapter feature narrowing
   - Status: implemented as the fourth split.
   - Move broad adapter feature enablement behind explicit binary/runtime feature sets.
   - Keep provider/auth and command-contract tests from compiling every gateway adapter by default.
   - Default `hermes-cli` builds still enable `gateway-adapters-all` to preserve installed-user behavior; `--no-default-features` now compiles the gateway core without adapter modules and emits clear skip diagnostics for enabled-but-uncompiled adapters.

5. Parity test dependency narrowing
   - Status: second split implemented.
   - Command-contract parity now lives in `hermes-source-parity-tests` and reads the CLI source contract without a `hermes-cli`, `clap`, fixture-harness, or protocol-stack dependency.
   - Protocol differential parity now lives in `hermes-protocol-parity-tests`, isolating ACP, MCP, gateway, and tool-runtime dependencies to tests that need them.
   - `hermes-parity-tests` remains the Rust fixture harness crate for core/intelligence fixture parity.
   - Point behavioral/provider parity tests at `hermes-provider-runtime`, `hermes-app-runtime`, and command-contract crates where possible.
   - Keep full `hermes-cli` parity checks for end-to-end CLI behavior only.

6. Runtime tool-planning policy
   - Status: implemented as the fifth split.
   - Owns platform alias normalization, default/configured platform toolset selection, coding-focus narrowing, live MCP toolset inclusion, explicit tool enable/disable merging, schema filtering, and compact tool-definition summaries.
   - Uses `hermes-config` defaults as the source of truth and extends them only for runtime-only `api_server` tool planning.
   - Keeps `hermes-cli::platform_toolsets` as an explicit compatibility re-export while moving tests and implementation to `hermes-tool-planning`.

## Gates

- `scripts/audit-cargo-build-surface.sh`
- `cargo test -p hermes-provider-runtime`
- `cargo test -p hermes-app-runtime`
- `cargo test -p hermes-tool-planning`
- `cargo test -p hermes-source-parity-tests --test global_parity_governance -- --nocapture`
- `cargo test -p hermes-protocol-parity-tests --test protocol_differential_contracts -- --nocapture`
- `cargo build -p hermes-cli --bin hermes-ultra --bin hermes-agent-ultra`

## Non-Goals

- Do not split crates just to reduce a dependency count if it makes runtime behavior harder to audit.
- Do not change public CLI behavior during the build-surface split.
- Do not move gateway adapter behavior out of Rust or behind Python fallback.
