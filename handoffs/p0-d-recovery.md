# P0-D: unmatched-upgrade recovery and secret-safe configuration output

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
Pre-edit provenance verdict: no candidate branch should be cherry-picked wholesale. Patch-ID plus whole-tree comparison showed all unmatched local/origin candidates were already equivalent or semantically superseded except two useful behaviors from `origin/test-parity-command-behavior`: secret-safe configuration output and optional session `--limit`. Query-slash handling and the old sessions renderer were superseded by newer main behavior.

## Intentional changes

- Character-safe redaction for secret values.
- URL credential redaction in top-level model plus delegation, LLM, auxiliary, smart-model, and OAuth token URL fields when credentials are present.
- Configuration and CLI end-to-end regressions proving model URL set/get/show paths do not leak credentials.
- Stale API-key display expectations updated from `***` to `[REDACTED]`.
- Public `proxy.http`/`proxy.socks` CLI aliases persist under the typed serde field names so set/get/show round-trips remain secret-safe and lossless.
- Session `--limit` work is retained only if it is implemented through the current canonical post-sort renderer; never restore the stale branch renderer.

## Files touched

- `crates/hermes-config/src/loader/user_config_patch.rs`
- `crates/hermes-config/src/loader/config_write.rs`
- `crates/hermes-config/src/loader/tests/user_config_patch.rs`
- `crates/hermes-cli/tests/e2e_cli.rs`
- related CLI configuration display tests and expectations

## Verification completed

Final focused checks passed with `CARGO_BUILD_JOBS=4`, `CARGO_INCREMENTAL=0`, and an isolated target directory; use `CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target/p0-eval}` for a portable reproduction:

- `cargo test -p hermes-config --lib -- --nocapture`: 204 passed, 0 failed.
- `cargo test -p hermes-cli --test e2e_cli e2e_cli_config -- --nocapture`: 4 passed, 0 failed.
- `cargo test -p hermes-cli sessions -- --nocapture`: 16 passed, 0 failed across matching unit and E2E binaries.
- `cargo test -p hermes-cli mask_secret_value -- --nocapture`: 2 passed, 0 failed.
- Aggregate: 226 passed, 0 failed in 237.92 seconds measured wall time.

## Remaining risks

- Generated parity/release branches are stale evidence and must not be recovered as source changes.
- Re-run the focused redaction and sessions tests after later CLI/config integrations.
