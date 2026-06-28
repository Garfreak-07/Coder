class CoderRust < Formula
  desc "Rust-first Coder control plane"
  homepage "https://github.com/Garfreak-07/Coder"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Garfreak-07/Coder/releases/download/v0.1.0/coder-rust-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    else
      url "https://github.com/Garfreak-07/Coder/releases/download/v0.1.0/coder-rust-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/Garfreak-07/Coder/releases/download/v0.1.0/coder-rust-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_X86_64_LINUX_GNU_SHA256"
    end
  end

  def install
    bin.install "coder-rust"
    pkgshare.install "README.md" if File.exist?("README.md")
    pkgshare.install "LICENSE" if File.exist?("LICENSE")
    pkgshare.install "examples" if Dir.exist?("examples")
  end

  test do
    assert_match "coder-rust", shell_output("#{bin}/coder-rust --help")
  end
end
