#!/usr/bin/env bash
# Build and package the MVP M2Image templates.
#
# This is the single, reproducible entry point for a release builder:
#
#   ./scripts/build-m2images.sh
#
# It downloads only official Ubuntu/Alpine/Rocky inputs through the existing
# distro builders, writes Firecracker-ready artifacts beneath `images/`, then
# produces `dist/m2images/{alias}.tar.zst` and verifies `SHA256SUMS`.
# Ubuntu's builder uses a temporary chroot, so it will request sudo when this
# command reaches that step. The API service is deliberately not involved.

set -euo pipefail

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH=; cd -- "${script_dir}/.." && pwd -P)
out_dir=${OUT_DIR:-}
alias_filter=all

usage() {
  cat <<'EOF'
Usage: ./scripts/build-m2images.sh [--alias alpine-3.24|ubuntu-26.04|rocky-9.8]

Builds host-native Firecracker template(s), packages them into OUT_DIR, and
verifies OUT_DIR/SHA256SUMS. x86_64 defaults to dist/m2images/x86_64; ARM64 defaults
to dist/m2images/aarch64.

The full MVP release build is:
  ./scripts/build-m2images.sh

Ubuntu needs sudo for its temporary chroot. Alpine and Rocky build in Docker.
Rocky Linux 9.8 builds on both x86_64 and ARM64.
EOF
}

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --alias)
      [ "$#" -ge 2 ] || fail 'missing value for --alias'
      alias_filter=$2
      shift 2
      ;;
    --alias=*)
      alias_filter=${1#--alias=}
      shift
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

case "$alias_filter" in
  all|alpine-3.24|ubuntu-26.04|rocky-9.8) ;;
  *) fail "unknown --alias $alias_filter (want alpine-3.24, ubuntu-26.04, or rocky-9.8)" ;;
esac

case "$(uname -m)" in
  x86_64|amd64) m2image_arch=x86_64 ;;
  aarch64|arm64) m2image_arch=aarch64 ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

if [ -z "$out_dir" ]; then
  if [ "$m2image_arch" = aarch64 ]; then
    out_dir="${repo_dir}/dist/m2images/aarch64"
  else
    out_dir="${repo_dir}/dist/m2images/x86_64"
  fi
fi

case "$alias_filter" in
  all|alpine-3.24|rocky-9.8)
    command -v docker >/dev/null 2>&1 || fail 'docker is required for the selected image builder(s)'
    ;;
esac
command -v sha256sum >/dev/null 2>&1 || fail 'sha256sum is required'

build_alpine() {
  info 'building alpine-3.24 from the official Alpine minirootfs and linux-virt packages'
  "${script_dir}/firecracker-menual/install-alpine-rootfs.sh"
}

build_ubuntu() {
  info 'building ubuntu-26.04 from Ubuntu Base and linux-image-generic'
  "${script_dir}/firecracker-menual/install-ubuntu-roofs.sh"
}

build_rocky() {
  info "building rocky-9 (Rocky Linux 9.8 ${m2image_arch}) from official BaseOS/AppStream packages and kernel"
  "${script_dir}/firecracker-menual/install-rocky-rootfs.sh"
}

case "$alias_filter" in
  all)
    build_alpine
    build_ubuntu
    build_rocky
    ;;
  alpine-3.24)
    build_alpine
    ;;
  ubuntu-26.04)
    build_ubuntu
    ;;
  rocky-9.8)
    build_rocky
    ;;
esac

info "packaging ${alias_filter} into ${out_dir}"
IMAGE_ROOT="${repo_dir}/images" OUT_DIR="$out_dir" M2IMAGE_ARCH="$m2image_arch" \
  "${script_dir}/package-m2images.sh" --alias "$alias_filter"

info "verifying ${out_dir}/SHA256SUMS"
(
  cd "$out_dir"
  sha256sum -c SHA256SUMS
)

info 'M2Image package build complete'
