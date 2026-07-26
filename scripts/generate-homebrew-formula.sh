#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-v0.13.0}"
REPO="https://github.com/sheawinkler/hermes-agent-ultra"
FORMULA_DIR="Formula"
FORMULA_FILE="${FORMULA_DIR}/hermes-agent-ultra.rb"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "Missing required command: shasum or sha256sum" >&2
    exit 1
  fi
}

MACOS_AARCH64_SHA="$(sha256_file dist/hermes-macos-aarch64.tar.gz)"
MACOS_X86_64_SHA="$(sha256_file dist/hermes-macos-x86_64.tar.gz)"
LINUX_AARCH64_SHA="$(sha256_file dist/hermes-linux-aarch64.tar.gz)"
LINUX_X86_64_SHA="$(sha256_file dist/hermes-linux-x86_64.tar.gz)"

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
      sha256 "${MACOS_AARCH64_SHA}"
    else
      url "${REPO}/releases/download/${VERSION}/hermes-macos-x86_64.tar.gz"
      sha256 "${MACOS_X86_64_SHA}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${REPO}/releases/download/${VERSION}/hermes-linux-aarch64.tar.gz"
      sha256 "${LINUX_AARCH64_SHA}"
    else
      url "${REPO}/releases/download/${VERSION}/hermes-linux-x86_64.tar.gz"
      sha256 "${LINUX_X86_64_SHA}"
    end
  end

  def install
    bin.install "hermes" => "hermes-agent-ultra"
    bin.install_symlink "hermes-agent-ultra" => "hermes-ultra"
  end

  test do
    system "#{bin}/hermes-agent-ultra", "--version"
  end
end
EOF

echo "Generated ${FORMULA_FILE} for ${VERSION}"
