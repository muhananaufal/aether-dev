# Homebrew formula for a tap. See packaging/README.md for what to fill in.
#
# Prebuilt binaries rather than building from source: the whole dependency tree
# is pure Rust, but a from-source install still asks everybody to compile it,
# and the release workflow has already done that on the same architecture.
class Adev < Formula
  desc "Terminal dashboard for a local development environment"
  homepage "https://github.com/muhananaufal/aether-dev"
  version "VERSION"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/muhananaufal/aether-dev/releases/download/v#{version}/adev-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_MACOS_ARM"
    end
    on_intel do
      url "https://github.com/muhananaufal/aether-dev/releases/download/v#{version}/adev-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_MACOS_INTEL"
    end
  end

  on_linux do
    on_intel do
      # The musl build, which is static and does not care how old the
      # distribution's glibc is.
      url "https://github.com/muhananaufal/aether-dev/releases/download/v#{version}/adev-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "SHA256_LINUX"
    end
  end

  def install
    bin.install "adev"
    doc.install "README.md", "aether.example.toml"
  end

  def caveats
    <<~EOS
      adev reads its configuration from aether.toml in the current directory or
      any parent, and failing that from ~/.config/aether-dev/aether.toml.

      To write one describing this machine:
        adev config --init
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/adev --version")
  end
end
