#!/usr/bin/env bash

set -euo pipefail

releases_url='https://github.com/firecracker-microvm/firecracker/releases'
install_dir='/usr/local/bin'

tmpdir=''

info() {
  printf '[INFO] %s\n' "$1"
}

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  exit 1
}

has_command() {
  command -v "$1" >/dev/null 2>&1
}

cleanup() {
  if [ -n "$tmpdir" ] && [ -d "$tmpdir" ]; then
    rm -rf "$tmpdir"
  fi
}

trap cleanup EXIT

require_command() {
  if ! has_command "$1"; then
    fail "Required command not found: $1"
  fi
}

run_install() {
  if [ -w "$install_dir" ]; then
    "$@"
    return
  fi

  if has_command sudo; then
    sudo "$@"
    return
  fi

  fail "Install directory is not writable and sudo is not available: ${install_dir}"
}

detect_arch() {
  case "$(uname -m 2>/dev/null || printf 'unknown')" in
    x86_64 | amd64)
      printf '%s\n' 'x86_64'
      ;;
    aarch64 | arm64)
      printf '%s\n' 'aarch64'
      ;;
    *)
      fail 'Unsupported architecture. Firecracker release binaries support x86_64 and aarch64.'
      ;;
  esac
}

resolve_version() {
  latest_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "${releases_url}/latest")
  latest_version="${latest_url##*/}"

  if [ -z "$latest_version" ] || [ "$latest_version" = 'latest' ]; then
    fail 'Could not resolve the latest Firecracker release version.'
  fi

  printf '%s\n' "$latest_version"
}

main() {
  # Check the local tools needed to download, extract, install, and verify Firecracker.
  require_command curl
  require_command find
  require_command install
  require_command mktemp
  require_command tar
  require_command uname

  # Resolve the host architecture and the Firecracker release version to install.
  arch=$(detect_arch)
  version=$(resolve_version)
  archive="firecracker-${version}-${arch}.tgz"
  download_url="${releases_url}/download/${version}/${archive}"

  info "architecture: ${arch}"
  info "version: ${version}"
  info "install directory: ${install_dir}"

  # Download the Firecracker release archive into a temporary working directory.
  tmpdir=$(mktemp -d)
  archive_path="${tmpdir}/${archive}"
  info "downloading: ${download_url}"
  curl -fsSL "$download_url" -o "$archive_path"

  # Extract the release archive and locate the Firecracker executable inside it.
  info 'extracting release archive'
  tar -xzf "$archive_path" -C "$tmpdir"
  binary_path=$(find "$tmpdir" -type f -name "firecracker-${version}-${arch}" -print -quit)

  if [ -z "$binary_path" ]; then
    fail "Firecracker binary was not found in archive: ${archive}"
  fi

  # Install the executable and set the final file mode to be runnable by users.
  target_path="${install_dir}/firecracker"
  info "installing binary: ${target_path}"
  run_install install -d -m 0755 "$install_dir"
  run_install install -m 0755 "$binary_path" "$target_path"

  # Verify that the installed binary is executable and reports its version.
  if [ ! -x "$target_path" ]; then
    fail "Installed binary is not executable: ${target_path}"
  fi

  info 'installed Firecracker version:'
  "$target_path" --version
}

main "$@"
