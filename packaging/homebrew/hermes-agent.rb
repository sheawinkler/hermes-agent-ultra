class HermesAgentUltra < Formula
  desc "Hermes Agent Ultra: autonomous AI agent with memory, tools, and gateway adapters"
  homepage "https://github.com/sheawinkler/hermes-agent-ultra"
  # Update URL and sha256 for each release
  url "https://github.com/sheawinkler/hermes-agent-ultra/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_ACTUAL_SHA256"
  license "MIT"
  head "https://github.com/sheawinkler/hermes-agent-ultra.git", branch: "main"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  depends_on "openssl"
  depends_on "sqlite"

  def install
    system "cargo", "install", "--path", "crates/hermes-cli", "--locked", "--root", prefix, "--bins"
  end

  def post_install
    (var/"hermes").mkpath
  end

  def caveats
    <<~EOS
      Hermes Agent Ultra has been installed.

      Set your API key before running:
        export HERMES_OPENAI_API_KEY="sk-..."

      Data is stored in:
        #{var}/hermes

      Get started:
        hermes-ultra --help
        hermes-ultra
    EOS
  end

  test do
    assert_match "hermes-agent-ultra", shell_output("#{bin}/hermes-agent-ultra --version")
  end
end
