class HermesAgentUltra < Formula
  desc "Hermes Agent Ultra autonomous AI agent"
  homepage "https://github.com/sheawinkler/hermes-agent-ultra"
  version "0.14.2"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/sheawinkler/hermes-agent-ultra/releases/download/v0.14.2/hermes-macos-aarch64.tar.gz"
      sha256 "71ab5172a0ba2672bd66ab4742c73b99649adaeba131df29a6a08987059953c8"
    else
      url "https://github.com/sheawinkler/hermes-agent-ultra/releases/download/v0.14.2/hermes-macos-x86_64.tar.gz"
      sha256 "ae8472ad3a724de13897ed2a9ec808024a0b732353fcf28b45ecdf1ab821f4e8"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/sheawinkler/hermes-agent-ultra/releases/download/v0.14.2/hermes-linux-aarch64.tar.gz"
      sha256 "75a5b2751c38b131b3c244504ec69c37e2a7799259cee26bb6e13e695dcf0529"
    else
      url "https://github.com/sheawinkler/hermes-agent-ultra/releases/download/v0.14.2/hermes-linux-x86_64.tar.gz"
      sha256 "f22d6fb401c0a1ef75a4cd873079063a5f9e2d83679716c4ac911c2db11fd30d"
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
