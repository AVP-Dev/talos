# Typed: false
# frozen_string_literal: true

class Talos < Formula
  desc "Local CLI orchestrator for AI coding agents with Team and Pair modes"
  homepage "https://talos.avpdev.com"
  version "0.1.1"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.1/talos-v0.1.1-aarch64-apple-darwin.tar.gz"
      sha256 "8f0a6045cd177730f55289a836e4e4fc22caa1a09941833db8230e719f5a8010"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.1/talos-v0.1.1-x86_64-apple-darwin.tar.gz"
      sha256 "a1feb301efa3f9d88dec30a71538f3f4cd8ef234381cb183d91f1badd976b88e"

      def install
        bin.install "talos"
      end
    end
  end

  on_linux do
    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.1/talos-v0.1.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "fa4a51fff0fde45b9b902401bd837f3c2be8024638657ac1cd99df68921c509b"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.1/talos-v0.1.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "54a5a0129df6ce5304bda4a4df797f5886224e5e1185daf80851083636765100"

      def install
        bin.install "talos"
      end
    end
  end

  test do
    assert_match "talos 0.1.1", shell_output("#{bin}/talos --version")
  end
end
