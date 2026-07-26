# P0: durable scoped DM authorization grants

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`, seeded with the exact live dirty baseline before isolated work.
Pre-edit verdict: confirmed access-control gap. Existing authorization state did not provide a durable, versioned grant bound to platform, DM chat, user, operation, and expiry; broad source authorization could escape its intended scope.

## Integrated implementation

- Versioned v1 grant schema binds canonical platform, exact DM chat, user, direct-message scope, direct-message operation, platform-local admin origin, issuance, expiry, and revocation tombstone.
- Eight-character pending codes resolve the canonical platform/chat/user target and are persisted before exposure. Approval, denial, and revocation stage the complete grant/pending snapshot before mutating memory.
- Owner-only `dm-grants.json` lives under the CLI canonical Hermes state root. The CLI calls `build_gateway_dm_manager_with_grant_store(&config, gateway_dm_grant_store_path(&cli))?` before `Gateway::new`.
- Existing live ingress preserves platform policy before deduplication, then calls `DmManager::handle_dm_for_chat(user_id, platform, chat_id, is_slash_command)`. That is the production path that enforces exact durable grant scope; grants cannot authorize slash or group traffic.
- Atomic replacement syncs the temporary file and parent directory; post-replace failures roll back before memory commit. Filesystems without hard-link support use a same-directory owner-only copied and fsynced rollback snapshot.
- Missing storage starts empty; malformed, unsupported-version, duplicate, invalid, insecure, or unscoped legacy storage fails closed. There is deliberately no permissive legacy `approved_dm_users` migration.
- Windows rejects any configured durable store before load until a verified owner-only ACL implementation exists.

## Exact integrated P0-B file delta

- `crates/hermes-gateway/src/dm.rs`
- `crates/hermes-gateway/src/gateway/routing_methods.rs`
- `crates/hermes-gateway/src/gateway/slash_command_methods.rs`
- `crates/hermes-gateway/src/commands.rs`
- `crates/hermes-gateway/src/gateway/tests/status_profiles.rs`
- `crates/hermes-cli/src/main/gateway_access.rs`
- `crates/hermes-cli/src/main/gateway_command.rs`
- `crates/hermes-cli/src/main/tests.rs`

## Manifest reconciliation

- The earlier seven-file receipt was an accounting error: it incorrectly added `crates/hermes-gateway/Cargo.toml` and omitted `gateway/routing_methods.rs` and `commands.rs`.
- Direct byte comparison proved `hermes-gateway/Cargo.toml` is identical between the isolated and live worktrees, so it is not part of this packet.
- `routing_methods.rs` is required to supply the exact chat and slash context to `handle_dm_for_chat`; its live P0-C policy-before-dedup ordering/comment remains intact.
- `commands.rs` is required to parse and describe `/approve <PAIRING_CODE>` and `/deny <PAIRING_CODE>` instead of arbitrary user IDs.

## Verification state

- Direct isolated-to-live comparison matches all seven non-routing files exactly. The routing comparison differs only because live retains the P0-C policy-before-dedup comment/blank line; the reviewed contextual authorization hunk is present.
- No merge conflict markers remain. Production `/approve` and `/deny` dispatch resolves pairing codes into scoped durable mutations; compatibility `authorize_user`/`deauthorize_user` helpers remain for configured/test state and are not the command path.
- Main verification passed: 26 DM durability/scope tests, 100 gateway authorization/queue/lifecycle scenarios, and 2 CLI canonical-root/fail-closed tests; 128 unique tests, 0 failures.
- The first full gateway run exposed three stale tests that inferred authorization from a message left behind after handler failure. P0-C correctly rolls failed turns back; those tests now use a successful handler and adapter and assert real dispatch. The 100-test gateway rerun passed.
- Measured reproduction latency for the nonredundant four-command suite was 295.06 seconds on the shared cold/nonincremental target.

## Focused checks for Main

- `cargo test -p hermes-gateway dm::tests -- --nocapture`
- `cargo test -p hermes-gateway gateway::tests::gateway_persistence_failures_emit_no_approval_or_revocation_success -- --nocapture`
- `cargo test -p hermes-cli gateway_dm_grant_store_path_uses_cli_state_root -- --nocapture`
- `cargo test -p hermes-cli gateway_dm_manager_fails_closed_when_durable_grant_store_is_invalid -- --nocapture`
