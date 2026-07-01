#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/../.." && pwd -P)

firecracker_bin='firecracker'
config_path="${repo_dir}/firecracker-config.json"
console_log_path="${repo_dir}/firecracker-console.log"
api_socket='/tmp/firecracker.socket'
vcpu_count='1'
mem_size_mib='512'
rootfs_read_only='false'
default_net_dev_name='tap0'
net_dev_name=''
net_iface_id='eth0'
net_guest_mac='06:00:AC:10:14:02'

info() {
  printf '[INFO] %s\n' "$1"
}

warn() {
  printf '[WARN] %s\n' "$1" >&2
}

fail() {
  printf '[FAIL] %s\n' "$1" >&2
  if [ -n "${console_log_path:-}" ]; then
    printf '[FAIL] %s\n' "$1" >>"$console_log_path" || true
  fi
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  ./scripts/firecracker-menual/boot-microvm.sh <kernel-image> <rootfs-image>
EOF
}

has_command() {
  command -v "$1" >/dev/null 2>&1
}

require_command() {
  if ! has_command "$1"; then
    fail "Required command not found: $1"
  fi
}

is_positive_integer() {
  case "$1" in
    '' | *[!0-9]* | 0)
      return 1
      ;;
    *)
      return 0
      ;;
  esac
}

abs_existing_file() {
  path=$1

  if [ ! -f "$path" ]; then
    fail "File does not exist: $path"
  fi

  dir=$(dirname "$path")
  file=$(basename "$path")
  printf '%s/%s\n' "$(cd "$dir" && pwd -P)" "$file"
}

prepare_output_path() {
  path=$1
  dir=$(dirname "$path")
  file=$(basename "$path")

  mkdir -p "$dir"
  printf '%s/%s\n' "$(cd "$dir" && pwd -P)" "$file"
}

resolve_executable() {
  executable=$1

  case "$executable" in
    */*)
      if [ ! -x "$executable" ]; then
        fail "Firecracker executable is not runnable: $executable"
      fi
      abs_existing_file "$executable"
      ;;
    *)
      command -v "$executable" || fail "Firecracker executable not found: $executable"
      ;;
  esac
}

json_escape() {
  value=$1
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  value=${value//$'\t'/\\t}
  printf '%s' "$value"
}

default_boot_args() {
  args='console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw'

  case "$(uname -m 2>/dev/null || printf 'unknown')" in
    aarch64 | arm64)
      args="keep_bootcon ${args}"
      ;;
  esac

  printf '%s\n' "$args"
}

check_kvm_access() {
  # Confirm that the host can expose KVM to Firecracker before booting.
  if [ ! -e /dev/kvm ]; then
    fail '/dev/kvm does not exist. Run the KVM readiness check before booting a microVM.'
  fi

  if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    fail 'Current user cannot read and write /dev/kvm.'
  fi
}

prepare_api_socket() {
  # Remove only a stale Unix socket; do not delete arbitrary files at that path.
  if [ -S "$api_socket" ]; then
    rm -f "$api_socket"
    return
  fi

  if [ -e "$api_socket" ]; then
    fail "API socket path already exists and is not a socket: ${api_socket}"
  fi
}

check_network_inputs() {
  if [ ! -e "/sys/class/net/${default_net_dev_name}" ]; then
    warn "network: host ${default_net_dev_name} does not exist; booting without a network device"
    warn 'network: create it with: sudo ip tuntap add dev tap0 mode tap user "$(id -un)"'
    warn 'network: then run: sudo ip addr replace 172.16.20.1/24 dev tap0'
    warn 'network: then run: sudo ip link set tap0 up'
    return
  fi

  net_dev_name=$default_net_dev_name
}

write_config() {
  # Write the Firecracker configuration used by --config-file boot.
  escaped_kernel_path=$(json_escape "$kernel_path")
  escaped_rootfs_path=$(json_escape "$rootfs_path")
  escaped_boot_args=$(json_escape "$boot_args")
  escaped_net_iface_id=$(json_escape "$net_iface_id")
  escaped_net_guest_mac=$(json_escape "$net_guest_mac")
  escaped_net_dev_name=$(json_escape "$net_dev_name")
  network_config=''

  if [ -n "$net_dev_name" ]; then
    network_config=$(cat <<EOF_NET
,
  "network-interfaces": [
    {
      "iface_id": "${escaped_net_iface_id}",
      "guest_mac": "${escaped_net_guest_mac}",
      "host_dev_name": "${escaped_net_dev_name}"
    }
  ]
EOF_NET
)
  fi

  cat >"$config_path" <<EOF
{
  "boot-source": {
    "kernel_image_path": "${escaped_kernel_path}",
    "boot_args": "${escaped_boot_args}"
  },
  "drives": [
    {
      "drive_id": "rootfs",
      "path_on_host": "${escaped_rootfs_path}",
      "is_root_device": true,
      "is_read_only": ${rootfs_read_only}
    }
  ]${network_config},
  "machine-config": {
    "vcpu_count": ${vcpu_count},
    "mem_size_mib": ${mem_size_mib},
    "smt": false
  }
}
EOF
}

verify_boot_log() {
  # Confirm that the captured console log reached userspace and did not hit kernel boot errors.
  if [ ! -s "$console_log_path" ]; then
    fail "Console log was not written: ${console_log_path}"
  fi

  if grep -Eiq "Kernel panic|not syncing|Unable to mount root fs|Cannot open root device|No working init found|Attempted to kill init|Run /bin/sh as init process|can't access tty; job control turned off" "$console_log_path"; then
    fail "Console log contains a guest boot error: ${console_log_path}"
  fi

  if ! grep -Eiq 'Welcome to|login:|root@[^[:space:]]+|Started .*Getty|Reached target .*Multi-User|Startup finished in' "$console_log_path"; then
    fail "Console log does not contain a userspace boot marker: ${console_log_path}"
  fi

  info "MicroVM userspace boot log detected: ${console_log_path}"
}

main() {
  if [ "$#" -ne 2 ]; then
    usage
    exit 1
  fi

  # Check local tools and host KVM access before creating the VM configuration.
  require_command basename
  require_command dirname
  require_command grep
  require_command mkdir
  require_command rm
  require_command tee
  require_command uname
  # Resolve outputs first so failures from this run do not leave stale logs.
  config_path=$(prepare_output_path "$config_path")
  console_log_path=$(prepare_output_path "$console_log_path")
  : >"$console_log_path"

  # Resolve all inputs to stable paths for the generated JSON config.
  firecracker_bin=$(resolve_executable "$firecracker_bin")
  kernel_path=$(abs_existing_file "$1")
  rootfs_path=$(abs_existing_file "$2")
  boot_args=$(default_boot_args)

  case "$rootfs_read_only" in
    true | false)
      ;;
    *)
      fail 'rootfs_read_only must be true or false.'
      ;;
  esac

  if ! is_positive_integer "$vcpu_count"; then
    fail 'vcpu_count must be a positive integer.'
  fi

  if ! is_positive_integer "$mem_size_mib"; then
    fail 'mem_size_mib must be a positive integer.'
  fi

  check_kvm_access
  check_network_inputs

  # Create the Firecracker config file.
  prepare_api_socket
  write_config

  info "config: ${config_path}"
  info "console log: ${console_log_path}"
  if [ -n "$net_dev_name" ]; then
    info "network: ${net_dev_name} -> ${net_iface_id} (${net_guest_mac})"
  fi
  info 'starting MicroVM; use the guest console to reboot or stop the VM when done'

  # Boot the MicroVM manually from the generated config and capture console output.
  set +e
  "$firecracker_bin" --api-sock "$api_socket" --config-file "$config_path" 2>&1 | tee "$console_log_path"
  firecracker_status=${PIPESTATUS[0]}
  set -e

  if [ "$firecracker_status" -ne 0 ]; then
    fail "Firecracker exited with status ${firecracker_status}."
  fi

  # Check the captured boot output after Firecracker exits.
  verify_boot_log
  info 'MicroVM boot completed successfully.'
}

main "$@"
