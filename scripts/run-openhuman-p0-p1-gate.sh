#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

required_commands=(
  "/commands"
  "/boot"
  "/walkthrough"
  "/triage"
  "/subconscious"
  "/integrations"
)

for cmd in "${required_commands[@]}"; do
  if ! rg -n --fixed-strings "\"${cmd}\"," crates/hermes-cli/src/commands >/dev/null; then
    echo "[gate] missing slash command registration: ${cmd}" >&2
    exit 1
  fi
done

required_tests=(
  "test_p0_p1_surface_commands_registered_and_completable"
  "p0_walkthrough_and_integrations_commands_emit_expected_sections"
  "p0_compress_rules_set_and_apply_updates_runtime_env"
  "p1_trigger_triage_escalates_high_severity_events"
  "test_format_tool_message_lines_truncates_large_payload"
)

for test_name in "${required_tests[@]}"; do
  if ! rg -n "fn ${test_name}\(" crates/hermes-cli/src/commands crates/hermes-cli/src/tui.rs crates/hermes-cli/src/tui >/dev/null; then
    echo "[gate] missing required test: ${test_name}" >&2
    exit 1
  fi
done

echo "[gate] static surface checks passed"

cargo test -p hermes-cli --lib test_p0_p1_surface_commands_registered_and_completable
cargo test -p hermes-cli --lib p0_walkthrough_and_integrations_commands_emit_expected_sections
cargo test -p hermes-cli --lib p0_compress_rules_set_and_apply_updates_runtime_env
cargo test -p hermes-cli --lib p1_trigger_triage_escalates_high_severity_events
cargo test -p hermes-cli --lib test_format_tool_message_lines_truncates_large_payload

echo "[gate] openhuman P0/P1 gate passed"
