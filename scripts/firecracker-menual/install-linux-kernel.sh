#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/../.." && pwd -P)

kernel_releases_url='https://www.kernel.org/releases.json'
artifact_dir="${repo_dir}/images/kernel"
build_dir="${repo_dir}/build/kernel-local"
jobs=''

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

require_command() {
  if ! has_command "$1"; then
    fail "Required command not found: $1"
  fi
}

ensure_writable_dir() {
  path=$1

  if [ ! -d "$path" ]; then
    fail "Directory does not exist: ${path}"
  fi

  if [ ! -w "$path" ]; then
    owner='unknown'
    if has_command stat; then
      owner=$(stat -c '%U:%G' "$path" 2>/dev/null || printf 'unknown')
    fi

    user_name=$(id -un 2>/dev/null || printf '%s' "${USER:-unknown}")
    group_name=$(id -gn 2>/dev/null || printf '%s' "$user_name")

    fail "Directory is not writable: ${path} (owner: ${owner}). Fix it with: sudo chown -R ${user_name}:${group_name} ${path}"
  fi
}

abs_dir() {
  path=$1

  mkdir -p "$path"
  cd "$path" && pwd -P
}

detect_host_arch() {
  case "$(uname -m 2>/dev/null || printf 'unknown')" in
    x86_64 | amd64)
      printf '%s\n' 'x86_64'
      ;;
    aarch64 | arm64)
      printf '%s\n' 'aarch64'
      ;;
    *)
      fail 'Unsupported architecture. This script supports x86_64 and aarch64.'
      ;;
  esac
}

kernel_make_arch() {
  case "$1" in
    x86_64)
      printf '%s\n' 'x86'
      ;;
    aarch64)
      printf '%s\n' 'arm64'
      ;;
    *)
      fail "Unsupported kernel build architecture: $1"
      ;;
  esac
}

resolve_stable_kernel() {
  releases_json="${build_dir}/releases.json"
  releases_json_tmp="${releases_json}.tmp"

  mkdir -p "$build_dir"
  ensure_writable_dir "$build_dir"
  if ! curl -fsSL "$kernel_releases_url" -o "$releases_json_tmp"; then
    rm -f "$releases_json_tmp"
    fail "Could not download kernel release metadata: ${kernel_releases_url}"
  fi
  mv "$releases_json_tmp" "$releases_json"

  python3 - "$releases_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)

version = data.get("latest_stable", {}).get("version")
source = None

for release in data.get("releases", []):
    if release.get("moniker") == "stable" and release.get("version") == version:
        source = release.get("source")
        break

if not version or not source:
    raise SystemExit("could not resolve latest stable kernel source")

print(f"{version} {source}")
PY
}

configure_firecracker_kernel() {
  config_tool="${source_dir}/scripts/config"

  if [ ! -x "$config_tool" ]; then
    fail "Kernel config helper is not executable: ${config_tool}"
  fi

  # Enable the kernel features needed for a small Firecracker VM with an ext4 root block device.
  "$config_tool" --file "${kernel_build_dir}/.config" \
    --enable BLOCK \
    --enable BLK_DEV \
    --enable DEVTMPFS \
    --enable DEVTMPFS_MOUNT \
    --enable EXT4_FS \
    --enable SERIAL_8250 \
    --enable SERIAL_8250_CONSOLE \
    --enable TTY \
    --enable UNIX \
    --enable VIRTIO \
    --enable VIRTIO_BLK \
    --enable VIRTIO_MMIO \
    --enable VIRTIO_MMIO_CMDLINE_DEVICES

  if [ "$host_arch" = 'aarch64' ]; then
    "$config_tool" --file "${kernel_build_dir}/.config" \
      --enable SERIAL_AMBA_PL011 \
      --enable SERIAL_AMBA_PL011_CONSOLE
  fi
}

main() {
  # Check local tools before downloading and building the stable Linux kernel.
  require_command curl
  require_command id
  require_command install
  require_command make
  require_command mkdir
  require_command mv
  require_command python3
  require_command tar
  require_command uname
  require_command xz

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  ensure_writable_dir "$build_dir"
  ensure_writable_dir "$artifact_dir"
  host_arch=$(detect_host_arch)
  make_arch=$(kernel_make_arch "$host_arch")
  kernel_info=$(resolve_stable_kernel)
  kernel_version=${kernel_info%% *}
  kernel_source_url=${kernel_info#* }

  if [ -z "$jobs" ]; then
    if has_command nproc; then
      jobs=$(nproc)
    else
      jobs=1
    fi
  fi

  download_dir="${build_dir}/downloads"
  archive_path="${download_dir}/linux-${kernel_version}.tar.xz"
  source_dir="${build_dir}/linux-${kernel_version}"
  kernel_build_dir="${build_dir}/linux-${kernel_version}-${host_arch}-build"
  versioned_image="${artifact_dir}/vmlinux-${kernel_version}-${host_arch}"

  info "kernel stable version: ${kernel_version}"
  info "host architecture: ${host_arch}"
  info "source URL: ${kernel_source_url}"

  # Download the latest stable kernel source archive from kernel.org.
  mkdir -p "$download_dir" "$artifact_dir"
  if [ ! -f "$archive_path" ]; then
    info "downloading kernel source: ${archive_path}"
    curl -fsSL "$kernel_source_url" -o "$archive_path"
  else
    info "reusing downloaded kernel source: ${archive_path}"
  fi

  # Extract the kernel source if it is not already present in the build directory.
  if [ ! -d "$source_dir" ]; then
    info "extracting kernel source: ${source_dir}"
    tar -xJf "$archive_path" -C "$build_dir"
  else
    info "reusing kernel source directory: ${source_dir}"
  fi

  # Build an uncompressed vmlinux image that Firecracker can boot with --config-file.
  if [ -f "${kernel_build_dir}/vmlinux" ]; then
    info "reusing existing kernel build: ${kernel_build_dir}"
  else
    info "configuring kernel build: ${kernel_build_dir}"
    mkdir -p "$kernel_build_dir"
    make -C "$source_dir" O="$kernel_build_dir" ARCH="$make_arch" defconfig
    configure_firecracker_kernel
    make -C "$source_dir" O="$kernel_build_dir" ARCH="$make_arch" olddefconfig

    info "building vmlinux with ${jobs} job(s)"
    make -C "$source_dir" O="$kernel_build_dir" ARCH="$make_arch" -j"$jobs" vmlinux
  fi

  # Install only the versioned kernel artifact.
  if [ ! -f "${kernel_build_dir}/vmlinux" ]; then
    fail "Kernel build did not produce vmlinux: ${kernel_build_dir}/vmlinux"
  fi

  install -m 0644 "${kernel_build_dir}/vmlinux" "$versioned_image"

  info "kernel versioned image: ${versioned_image}"

  if has_command file; then
    file "$versioned_image"
  fi
}

main "$@"
