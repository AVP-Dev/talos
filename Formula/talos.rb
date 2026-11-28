# Typed: false
# frozen_string_literal: true

class Talos < Formula
  desc "Local CLI orchestrator for AI coding agents with Team and Pair modes"
  homepage "https://talos.avpdev.com"
  version "0.1.2"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.2/talos-v0.1.2-aarch64-apple-darwin.tar.gz"
      sha256 "74f0898423076038cdaad4820fb5fbaf56dedb4968651b0a4e850c4f0821a86d"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.2/talos-v0.1.2-x86_64-apple-darwin.tar.gz"
      sha256 "746ad579386957ca6aa3d65cc14d413c32c378d190e43fc6d718914292976451"

      def install
        bin.install "talos"
      end
    end
  end

  on_linux do
    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.2/talos-v0.1.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "29dd2225bbc5f12e52476c8236590b4fae66fe91045052bab78ddb129ad324d7"

      def install
        bin.install "talos"
      end
    end
    if Hardware::CPU.intel?
      url "https://github.com/AVP-Dev/talos/releases/download/v0.1.2/talos-v0.1.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "acf850d5fbe3815322db03f6206f6a3078d0683614e1650f204e981e8bd85503"

      def install
        bin.install "talos"
      end
    end
  end

  test do
    assert_match "talos 0.1.2", shell_output("#{bin}/talos --version")
  end
end
