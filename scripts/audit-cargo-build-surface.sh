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
    /^hermes-gateway = / { in_gateway = 1; next }
    in_gateway && /^\] / { in_gateway = 0 }
    in_gateway && /^\]/ { in_gateway = 0 }
    in_gateway && /"/ {
      line = $0;
      while (match(line, /"[^"]+"/)) {
        count++;
        line = substr(line, RSTART + RLENGTH);
      }
    }
    END { print count + 0 }
  ' "$cli_manifest"
)"

echo "## Gateway Adapter Feature Surface"
echo
echo "- Feature count pulled into \`hermes-cli\`: \`$gateway_feature_count\`"
echo

echo "## cargo tree: hermes-cli --edges normal --depth 1"
echo
echo '```text'
cargo tree -p hermes-cli --edges normal --depth 1
echo '```'
echo

echo "## Interpretation"
echo
echo "- \`hermes-cli\` is currently the invalidation root for wrappers, the main runtime binary, provider/auth routing, TUI/clipboard UI dependencies, gateway adapter features, cron, ACP, MCP, tools, skills, and telemetry."
echo "- The first crate split should isolate provider/auth routing and noninteractive chat contracts from the CLI/TUI and gateway adapter feature surface."
echo "- Parity tests that only validate provider, auth, or command contracts should depend on that narrower crate instead of pulling the full CLI binary surface."
