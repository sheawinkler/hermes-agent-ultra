# P0: native Discord Gateway ingress

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`, seeded with the exact live dirty baseline before isolated work.
Pre-edit verdict: confirmed real product gap. Discord outbound/adapter surfaces existed, but there was no complete native Gateway v10 socket ingress driving verified sender provenance through `Gateway::route_message_from_sender`.

## Isolated implementation under correction

- One owner task and one live WebSocket; no runtime/task `Arc` cycle.
- Gateway v10 HELLO, heartbeat/ACK liveness, IDENTIFY/RESUME, READY session/self-ID capture, RECONNECT, INVALID_SESSION reset, bounded reconnect, and deterministic cancellation.
- MESSAGE_CREATE preserves server-verified human/bot/webhook provenance and routes through `Gateway::route_message_from_sender`.
- Platform `extra.allow_bots` resolves to the native Discord bot-message policy while preserving the environment fallback.
- Review-mandated closure is in progress: complete close-code/proxy/server-heartbeat/IDENTIFY-pacing semantics; bounded retained pending events; terminal closed consumer; real thread-parent/participation state; typed DM/group/thread session scope; `require_mention=true` default; and fail-closed channel/user/role authorization through the existing `DiscordInteractionAuthPolicy`.

## Intentional file delta

- `crates/hermes-gateway/src/platforms/discord.rs`
- `crates/hermes-gateway/src/platforms/discord/core_config.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_driver.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_state/adapter_struct.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_state/adapter_io.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_state/message_models.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_state/parsing.rs`
- `crates/hermes-gateway/src/platforms/discord/gateway_state/session.rs`
- `crates/hermes-gateway/src/platforms/discord/tests/gateway_protocol.rs`
- `crates/hermes-gateway/src/platforms/discord/tests/policy_and_history.rs`
- `crates/hermes-cli/src/main/gateway_adapters.rs`

## Required integration checks

- Review each file delta against the live dirty baseline; never copy the full worktree.
- `cargo test -p hermes-gateway --features discord gateway_driver_tests`
- `cargo test -p hermes-gateway --features discord gateway_protocol`
- `cargo test -p hermes-cli --features gateway-discord discord_bot_policy_wiring_tests`
- Saturated queue plus non-resumable invalid-session exact-once regression.
- Closed/missing consumer terminal regression.
- Close-code reset/fatal, proxy, server opcode-1 heartbeat, and cancellation-aware IDENTIFY pacing regressions.
- Real MESSAGE_CREATE thread/participation fixtures.
- No-allowlist default deny, explicit-open allow, role-only allow, wrong-role deny, and cross-platform denial through the full route.
- `cargo check -p hermes-cli --features gateway-discord`

## Remaining risks

- Integration is blocked until the isolated corrections and independent review are complete.
- No live Discord credential exercise yet.
- Sharding and privileged-intent provisioning remain deployment concerns.
