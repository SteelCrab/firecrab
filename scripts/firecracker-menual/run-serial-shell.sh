#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/../.." && pwd -P)

default_rootfs_image="${repo_dir}/images/rootfs/ubuntu-rootfs.ext4"
rootfs_image="$default_rootfs_image"
kernel_image=''

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  exit 1
}

default_kernel_image() {
  local candidate

  for candidate in "${repo_dir}"/images/kernel/vmlinux-[0-9]*; do
    if [ -f "$candidate" ]; then
      printf '%s\n' "$candidate"
      return
    fi
  done

  fail 'Kernel image does not exist. Run: ./scripts/firecracker-menual/install-linux-kernel.sh'
}

main() {
  if [ "$#" -gt 2 ]; then
    fail 'Usage: ./scripts/firecracker-menual/run-serial-shell.sh [kernel-image] [rootfs-image]'
  fi

  if [ "$#" -ge 1 ]; then
    kernel_image=$1
  fi

  if [ "$#" -eq 2 ]; then
    rootfs_image=$2
  fi

  if [ -z "$kernel_image" ]; then
    kernel_image=$(default_kernel_image)
  fi

  if [ ! -f "$kernel_image" ]; then
    fail "Kernel image does not exist: ${kernel_image}"
  fi

  if [ ! -f "$rootfs_image" ]; then
    fail "Ubuntu rootfs does not exist. Run: ./scripts/firecracker-menual/install-ubuntu-roofs.sh"
  fi

  exec "${script_dir}/boot-microvm.sh" "$kernel_image" "$rootfs_image"
}

main "$@"
