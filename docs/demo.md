# Hermes Agent Ultra Demo And Readiness Path

This is the shortest public proof path for the current release baseline. It is
intentionally release-artifact-first: if a visitor installs from GitHub Releases,
the same smoke should pass without using the local development tree.

## One Command

```bash
bash scripts/smoke-release-artifact.sh --version v0.21.2
```

What it proves:

- downloads the published `install.sh` from the `v0.21.2` GitHub release
- installs the published platform artifact into a temporary `bin` directory
- fails if the installer falls back to building from source
- verifies `hermes-agent-ultra` and `hermes-ultra`
- verifies an existing upstream `hermes` command is not clobbered by default
- checks setup help, auth status, memory status, provider route health,
  systems release/status, and one-true-harness tool registry entries

## Source Tree Confidence Path

```bash
python3 scripts/generate-release-readiness-summary.py --repo-root . --check
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-cli --test e2e_sota_workflow_replay -- --nocapture
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-cli harness_command_reports_issue_backed_cockpit_and_teach_skill -- --nocapture
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-cli test_simulate_command_is_registered_and_completable -- --nocapture
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-cli test_qos_and_eval_commands_are_registered -- --nocapture
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-cli runtime_parity_ops -- --nocapture
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo test -p hermes-http dashboard_oidc -- --nocapture
```

## Differentiator Proof Map

| Claim | Public surface | Deterministic proof |
| --- | --- | --- |
| Current parity baseline is closed | release-readiness summary | `scripts/generate-release-readiness-summary.py --check` |
| Published artifacts install cleanly | release artifact smoke | `scripts/smoke-release-artifact.sh --version v0.21.2` |
| Upstream `hermes` can coexist | installer smoke sentinel | `scripts/smoke-release-artifact.sh --version v0.21.2` |
| One-true-harness cockpit exists | `/harness`, `harness_cockpit` | `cargo test -p hermes-cli harness_command_reports_issue_backed_cockpit_and_teach_skill` |
| Tool simulator exists | `/simulate`, `tool_policy_simulate` | `cargo test -p hermes-cli test_simulate_command_is_registered_and_completable` |
| Time-travel/session replay exists | `/timetravel`, replay fixture | `cargo test -p hermes-cli --test e2e_sota_workflow_replay` |
| Live session eval harness exists | `/ops eval run` | `cargo test -p hermes-cli native_session_eval_harness_writes_compatible_report` |
| Memory fusion/status is visible | `/memory`, ContextLattice policy, memory provider status | `cargo test -p hermes-cli test_memory_command_is_registered_completable_and_cataloged` |
| Provider QoS diagnostics exist | `/qos`, route health, ops snapshot | `cargo test -p hermes-cli test_qos_and_eval_commands_are_registered` |
| Dashboard OIDC is Rust-native | `hermes-http` dashboard OIDC | `cargo test -p hermes-http dashboard_oidc` |

## Drift Automation

The scheduled `Parity Audit` workflow regenerates governance artifacts, writes a
public release-readiness summary, uploads it with the parity artifacts, and uses
that summary for issue comments when drift breaks the audit.

Future tag releases also run the release-artifact smoke after the sign/publish
job, so installers are checked against the actual uploaded assets rather than a
local build.
