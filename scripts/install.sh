#!/usr/bin/env bash
set -euo pipefail

version="latest"
repo="Garfreak-07/Coder"
install_dir="${HOME}/.local/bin"
temp_dir=""
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2:?missing value for --version}"
      shift 2
      ;;
    --repo)
      repo="${2:?missing value for --repo}"
      shift 2
      ;;
    --install-dir)
      install_dir="${2:?missing value for --install-dir}"
      shift 2
      ;;
    --temp-dir)
      temp_dir="${2:?missing value for --temp-dir}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h|--help)
      cat <<'USAGE'
Usage: scripts/install.sh [--version VERSION|latest] [--install-dir DIR] [--temp-dir DIR] [--dry-run]

Downloads a coder-rust release archive, verifies it contains the binary, and
installs it into a user-local bin directory.
USAGE
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

os="$(uname -s)"
arch="$(uname -m)"
case "${os}:${arch}" in
  Linux:x86_64)
    target="x86_64-unknown-linux-gnu"
    archive="coder-rust-x86_64-unknown-linux-gnu.tar.gz"
    binary="coder-rust"
    ;;
  Darwin:x86_64)
    target="x86_64-apple-darwin"
    archive="coder-rust-x86_64-apple-darwin.tar.gz"
    binary="coder-rust"
    ;;
  Darwin:arm64)
    target="aarch64-apple-darwin"
    archive="coder-rust-aarch64-apple-darwin.tar.gz"
    binary="coder-rust"
    ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64)
    target="x86_64-pc-windows-msvc"
    archive="coder-rust-x86_64-pc-windows-msvc.zip"
    binary="coder-rust.exe"
    ;;
  *)
    echo "Unsupported platform: ${os} ${arch}" >&2
    exit 1
    ;;
esac

if [[ "${version}" == "latest" ]]; then
  release_base="https://github.com/${repo}/releases/latest/download"
else
  release_base="https://github.com/${repo}/releases/download/${version}"
fi
asset_url="${release_base}/${archive}"

echo "coder-rust installer"
echo "Target: ${target}"
echo "Archive: ${archive}"
echo "InstallDir: ${install_dir}"
temp_base="${temp_dir:-${CODER_INSTALL_TMPDIR:-${TMPDIR:-/tmp}}}"
echo "TempDir: ${temp_base}"
echo "URL: ${asset_url}"

if [[ "${dry_run}" == "1" ]]; then
  echo "DryRun: no download or install performed."
  exit 0
fi

mkdir -p "${temp_base}"
tmp_dir="$(mktemp -d "${temp_base%/}/coder-rust-install.XXXXXXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT
archive_path="${tmp_dir}/${archive}"
extract_dir="${tmp_dir}/extract"
mkdir -p "${extract_dir}" "${install_dir}"

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "${asset_url}" -o "${archive_path}"
elif command -v wget >/dev/null 2>&1; then
  wget -q "${asset_url}" -O "${archive_path}"
else
  echo "curl or wget is required to download ${asset_url}" >&2
  exit 1
fi

case "${archive}" in
  *.zip)
    if ! command -v unzip >/dev/null 2>&1; then
      echo "unzip is required for ${archive}" >&2
      exit 1
    fi
    unzip -q "${archive_path}" -d "${extract_dir}"
    ;;
  *.tar.gz)
    tar -xzf "${archive_path}" -C "${extract_dir}"
    ;;
esac

binary_path="$(find "${extract_dir}" -type f -name "${binary}" | head -n 1)"
if [[ -z "${binary_path}" ]]; then
  echo "Archive did not contain ${binary}" >&2
  exit 1
fi

install -m 0755 "${binary_path}" "${install_dir}/${binary}"
echo "Installed ${binary} to ${install_dir}"
echo "Next: coder-rust doctor"
