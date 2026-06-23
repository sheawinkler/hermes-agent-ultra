#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cli_manifest="crates/hermes-cli/Cargo.toml"

echo "# Hermes Cargo Build Surface Audit"
echo

target_dir="$(
  cargo metadata --format-version=1 --no-deps \
    | grep -o '"target_directory":"[^"]*"' \
    | head -n 1 \
    | cut -d '"' -f 4
)"

echo "## Workspace"
echo
echo "- Root: \`$repo_root\`"
echo "- Cargo target directory: \`$target_dir\`"
echo

echo "## hermes-cli Targets"
echo
awk '
  /^\[\[bin\]\]/ { in_bin = 1; name = ""; path = ""; next }
  in_bin && /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
  in_bin && /^path = / {
    path = $0;
    sub(/^path = "/, "", path);
    sub(/"$/, "", path);
    printf("- `%s` -> `%s`\n", name, path);
    in_bin = 0;
  }
' "$cli_manifest"
echo

echo "## hermes-cli Direct Internal Dependencies"
echo
awk '
  /^\[dependencies\]/ { in_deps = 1; next }
  /^\[/ && in_deps { exit }
  in_deps && /^hermes-/ {
    dep = $1;
    printf("- `%s`\n", dep);
  }
' "$cli_manifest"
echo

gateway_feature_count="$(
  awk '
    /^gateway-adapters-all = \[/ { in_gateway = 1; next }
    in_gateway && /^\]/ { in_gateway = 0 }
    in_gateway && /"gateway-/ {
      line = $0;
      while (match(line, /"gateway-[^"]+"/)) {
        count++;
        line = substr(line, RSTART + RLENGTH);
      }
    }
    END { print count + 0 }
  ' "$cli_manifest"
)"

echo "## Gateway Adapter Feature Surface"
echo
echo "- Default \`gateway-adapters-all\` feature count: \`$gateway_feature_count\`"
echo "- Narrow gateway build path: \`cargo check -p hermes-cli --bin hermes-agent-ultra --no-default-features\`"
echo

echo "## cargo tree: hermes-cli --edges normal --depth 1"
echo
echo '```text'
cargo tree -p hermes-cli --edges normal --depth 1
echo '```'
echo

echo "## cargo tree: hermes-cli --no-default-features --edges normal --depth 1"
echo
echo '```text'
cargo tree -p hermes-cli --no-default-features --edges normal --depth 1
echo '```'
echo

echo "## cargo tree: hermes-parity-tests --edges dev --depth 1"
echo
echo '```text'
cargo tree -p hermes-parity-tests --edges dev --depth 1
echo '```'
echo

echo "## cargo tree: hermes-source-parity-tests --edges all --depth 1"
echo
echo '```text'
cargo tree -p hermes-source-parity-tests --edges all --depth 1
echo '```'
echo

echo "## cargo tree: hermes-protocol-parity-tests --edges dev --depth 1"
echo
echo '```text'
cargo tree -p hermes-protocol-parity-tests --edges dev --depth 1
echo '```'
echo

echo "## Interpretation"
echo
echo "- \`hermes-cli\` is currently the invalidation root for wrappers, the main runtime binary, TUI/clipboard UI dependencies, cron, ACP, MCP, tools, skills, and telemetry."
echo "- Gateway adapter features are now behind explicit \`gateway-*\` CLI features. Default builds keep \`gateway-adapters-all\` for behavior compatibility, while \`--no-default-features\` checks compile the gateway core without adapter modules."
echo "- Provider/auth routing now has a narrower home in \`hermes-provider-runtime\`; agent configuration, query-mode provider/model/env/tool policy, model remediation, noninteractive agent-loop wiring, reply extraction, and runtime prompt reformulation policy now have a narrower home in \`hermes-app-runtime\`."
echo "- Source/governance parity now lives in \`hermes-source-parity-tests\`, so command-contract checks avoid \`hermes-cli\`, \`clap\`, fixture-harness runtime crates, and protocol stack crates."
echo "- Protocol differential parity now lives in \`hermes-protocol-parity-tests\`, isolating ACP/MCP/gateway/tool dependencies to tests that need them."
echo "- The next high-value split is remaining reusable memory/context policy injection and tool-planning policy away from CLI/TUI, followed by extracting CLI UI/help presentation into a narrower module/crate."
echo "- Parity tests that only validate provider, auth, app-runtime, or command contracts should keep moving to narrower crates instead of pulling the full CLI binary surface."
