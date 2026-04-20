#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-v0.1.0}"
REPO="https://github.com/sheawinkler/hermes-agent-ultra"
FORMULA_DIR="Formula"
FORMULA_FILE="${FORMULA_DIR}/hermes-agent-ultra.rb"

mkdir -p "${FORMULA_DIR}"

cat > "${FORMULA_FILE}" <<EOF
class HermesAgentUltra < Formula
  desc "Hermes Agent Ultra autonomous AI agent"
  homepage "${REPO}"
  version "${VERSION#v}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "${REPO}/releases/download/${VERSION}/hermes-macos-aarch64.tar.gz"
      sha256 "REPLACE_WITH_SHA256"
    else
      url "${REPO}/releases/download/${VERSION}/hermes-macos-x86_64.tar.gz"
      sha256 "REPLACE_WITH_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${REPO}/releases/download/${VERSION}/hermes-linux-aarch64.tar.gz"
      sha256 "REPLACE_WITH_SHA256"
    else
      url "${REPO}/releases/download/${VERSION}/hermes-linux-x86_64.tar.gz"
      sha256 "REPLACE_WITH_SHA256"
    end
  end

  def install
    bin.install "hermes" => "hermes-agent-ultra"
    bin.install_symlink "hermes-agent-ultra" => "hermes"
  end

  test do
    system "#{bin}/hermes-agent-ultra", "--version"
  end
end
EOF

echo "Generated ${FORMULA_FILE} for ${VERSION}"
