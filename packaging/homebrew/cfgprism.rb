# Homebrew tap template (maintainer: copy to ilyaosovskoi/homebrew-tap/Formula/cfgprism.rb
# after the first cargo-dist release, then fill VERSION + SHA256 from the release page).
#
# Until the tap repo exists, install with:
#   brew install --build-from-source https://raw.githubusercontent.com/ilyaosovskoi/cfgprism/main/packaging/homebrew/cfgprism.rb
class Cfgprism < Formula
  desc "Lossless-ish config converter with explicit warnings"
  homepage "https://github.com/ilyaosovskoi/cfgprism"
  version "0.1.0" # FIXME: release version
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/ilyaosovskoi/cfgprism/releases/download/v0.1.0/cfgprism-aarch64-apple-darwin.tar.gz" # FIXME
      sha256 "FIXME"
    else
      url "https://github.com/ilyaosovskoi/cfgprism/releases/download/v0.1.0/cfgprism-x86_64-apple-darwin.tar.gz" # FIXME
      sha256 "FIXME"
    end
  end

  on_linux do
    url "https://github.com/ilyaosovskoi/cfgprism/releases/download/v0.1.0/cfgprism-x86_64-unknown-linux-gnu.tar.gz" # FIXME
    sha256 "FIXME"
  end

  def install
    bin.install "cfgprism"
  end

  test do
    assert_match "json", shell_output("#{bin}/cfgprism formats")
  end
end
