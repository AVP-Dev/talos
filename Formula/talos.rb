# Typed: false
# frozen_string_literal: true

class Talos < Formula
  desc "Local CLI orchestrator for AI coding agents with Team and Pair modes"
  homepage "https://talos.avpdev.com"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.0/talos-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "d4a17a9c7a4fecab08f8ff889098c5f906480dca65b79b9d8b84fb4ed368d320"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.0/talos-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"

      def install
        bin.install "talos"
      end
    end
  end

  on_linux do
    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.0/talos-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.0/talos-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"

      def install
        bin.install "talos"
      end
    end
  end

  test do
    assert_match "talos 0.1.0", shell_output("#{bin}/talos --version")
  end
end
