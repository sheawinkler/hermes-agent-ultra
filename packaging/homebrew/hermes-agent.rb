class HermesAgent < Formula
  desc "AI agent framework with tool use, memory, and multi-model support"
  homepage "https://github.com/nousresearch/hermes-agent-rust"
  # Update URL and sha256 for each release
  url "https://github.com/nousresearch/hermes-agent-rust/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_ACTUAL_SHA256"
  license "MIT"
  head "https://github.com/nousresearch/hermes-agent-rust.git", branch: "main"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  depends_on "openssl"
  depends_on "sqlite"

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/hermes-cli")
    bin.install "target/release/hermes" if File.exist?("target/release/hermes")
  end

  def post_install
    (var/"hermes").mkpath
  end

  def caveats
    <<~EOS
      Hermes Agent has been installed.

      Set your API key before running:
        export HERMES_OPENAI_API_KEY="sk-..."

      Data is stored in:
        #{var}/hermes

      Get started:
        hermes --help
        hermes chat
    EOS
  end

  test do
    assert_match "hermes", shell_output("#{bin}/hermes --version")
  end
end
