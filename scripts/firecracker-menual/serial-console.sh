#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/../.." && pwd -P)

rootfs_image="${repo_dir}/images/rootfs/ubuntu-rootfs.ext4"
kernel_image=''
console_log_path="${repo_dir}/firecracker-console.log"

info() {
  printf '[INFO] %s\n' "$1"
}

warn() {
  printf '[WARN] %s\n' "$1" >&2
}

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  ./scripts/firecracker-menual/serial-console.sh

No arguments boots the MicroVM and attaches this terminal to ttyS0.
If host tap0 exists, it is attached as the MicroVM network device.
EOF
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

resolve_inputs() {
  if [ -z "$kernel_image" ]; then
    kernel_image=$(default_kernel_image)
  fi

  if [ ! -f "$kernel_image" ]; then
    fail "Kernel image does not exist: ${kernel_image}"
  fi

  if [ ! -f "$rootfs_image" ]; then
    fail "Ubuntu rootfs does not exist. Run: ./scripts/firecracker-menual/install-ubuntu-roofs.sh"
  fi
}

boot_console() {
  resolve_inputs

  if [ ! -t 0 ]; then
    warn 'stdin is not a TTY; Firecracker may detach serial input. Run from a host terminal for manual shell access.'
  fi

  info "kernel: ${kernel_image}"
  info "rootfs: ${rootfs_image}"
  info "console log: ${console_log_path}"
  info 'guest prompt: root shell on ttyS0'
  info 'exit guest with: reboot -f'

  "${script_dir}/run-serial-shell.sh" "$kernel_image" "$rootfs_image"
}

main() {
  if [ "$#" -eq 0 ]; then
    boot_console
    return
  fi

  command_name=$1

  case "$command_name" in
    -h | --help | help)
      usage
      ;;
    *)
      usage
      fail "Unknown command: ${command_name}"
      ;;
  esac
}

main "$@"
