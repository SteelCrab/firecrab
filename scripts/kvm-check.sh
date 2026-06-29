#!/usr/bin/env bash

set -u

failures=0
warnings=0

pass() {
  printf '[PASS] %s\n' "$1"
}

warn() {
  warnings=$((warnings + 1))
  printf '[WARN] %s\n' "$1"
}

fail() {
  failures=$((failures + 1))
  printf '[FAIL] %s\n' "$1"
}

info() {
  printf '[INFO] %s\n' "$1"
}

has_command() {
  command -v "$1" >/dev/null 2>&1
}

print_header() {
  printf '%s\n' 'KVM host environment check'
  printf '%s\n' '=========================='
  printf '%s\n' 'This checks host readiness only. It does not check whether Firecracker is installed.'
}

check_platform() {
  printf '\n%s\n' '1. Host platform'

  kernel_name=$(uname -s 2>/dev/null || printf 'unknown')
  kernel_release=$(uname -r 2>/dev/null || printf 'unknown')
  machine_arch=$(uname -m 2>/dev/null || printf 'unknown')

  info "kernel: ${kernel_name} ${kernel_release}"
  info "architecture: ${machine_arch}"

  if [ "$kernel_name" = 'Linux' ]; then
    pass 'Linux kernel detected.'
  else
    fail 'KVM requires a Linux host.'
  fi

  case "$machine_arch" in
    x86_64 | amd64)
      pass 'Supported CPU architecture detected: x86_64.'
      ;;
    aarch64 | arm64)
      pass 'Supported CPU architecture detected: aarch64.'
      ;;
    *)
      fail "Unsupported or unknown CPU architecture: ${machine_arch}"
      ;;
  esac
}

check_cpu_virtualization() {
  printf '\n%s\n' '2. CPU virtualization'

  if [ ! -r /proc/cpuinfo ]; then
    fail '/proc/cpuinfo is not readable.'
    return
  fi

  machine_arch=$(uname -m 2>/dev/null || printf 'unknown')
  case "$machine_arch" in
    x86_64 | amd64)
      virt_count=$(grep -Eoc '(vmx|svm)' /proc/cpuinfo 2>/dev/null || true)
      if [ "${virt_count:-0}" -gt 0 ]; then
        pass "CPU virtualization flag found. count=${virt_count}"
      else
        fail 'CPU virtualization flag was not found. Expected vmx or svm in /proc/cpuinfo.'
      fi
      ;;
    aarch64 | arm64)
      warn 'Skipping vmx/svm CPU flag check on ARM. /dev/kvm availability is the decisive check.'
      ;;
    *)
      warn "Skipping CPU virtualization flag check for unsupported architecture: ${machine_arch}"
      ;;
  esac
}

check_kvm_device() {
  printf '\n%s\n' '3. /dev/kvm device'

  if [ ! -e /dev/kvm ]; then
    fail '/dev/kvm does not exist. KVM may be disabled or unavailable in this host.'
    return
  fi

  if [ ! -c /dev/kvm ]; then
    fail '/dev/kvm exists but is not a character device.'
  else
    pass '/dev/kvm exists as a character device.'
  fi

  if has_command stat; then
    mode=$(stat -c '%A %U %G' /dev/kvm 2>/dev/null || true)
    if [ -n "${mode:-}" ]; then
      info "/dev/kvm permission: ${mode}"
    fi
  else
    ls -l /dev/kvm 2>/dev/null || true
  fi
}

check_user_access() {
  printf '\n%s\n' '4. Current user access'

  current_user=$(id -un 2>/dev/null || printf 'unknown')
  info "current user: ${current_user}"
  info "groups: $(id -nG 2>/dev/null || printf 'unknown')"

  if [ ! -e /dev/kvm ]; then
    warn 'Cannot check user access because /dev/kvm does not exist.'
    return
  fi

  if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    pass 'Current user can read and write /dev/kvm.'
  else
    fail 'Current user cannot read and write /dev/kvm.'
    warn "Add the checked user to the kvm group, then log out and log back in: sudo usermod -aG kvm \"${current_user}\""
  fi

  if id -nG 2>/dev/null | tr ' ' '\n' | grep -qx 'kvm'; then
    pass 'Current user is in the kvm group.'
  else
    warn 'Current user is not in the kvm group. Direct ACL/root access may still work, but kvm group membership is the common setup.'
  fi
}

check_kvm_module() {
  printf '\n%s\n' '5. KVM kernel support'

  machine_arch=$(uname -m 2>/dev/null || printf 'unknown')

  if [ -d /sys/module/kvm ]; then
    pass 'kvm is present in /sys/module.'
  elif [ -r /proc/modules ] && grep -q '^kvm ' /proc/modules; then
    pass 'kvm module is listed in /proc/modules.'
  elif [ -e /dev/kvm ]; then
    info 'kvm is not listed as a loaded module, but /dev/kvm exists. KVM may be built into the kernel.'
  elif [ -r /proc/modules ]; then
    warn 'kvm module is not listed in /proc/modules.'
  else
    warn '/proc/modules is not readable and /sys/module/kvm is not present. Skipping kernel module check.'
  fi

  case "$machine_arch" in
    x86_64 | amd64)
      if [ -d /sys/module/kvm_intel ] || [ -d /sys/module/kvm_amd ]; then
        pass 'x86 vendor KVM module is present in /sys/module.'
      elif [ -r /proc/modules ] && grep -Eq '^kvm_intel |^kvm_amd ' /proc/modules; then
        pass 'x86 vendor KVM module is listed in /proc/modules.'
      elif [ -e /dev/kvm ]; then
        info 'x86 vendor KVM module is not listed, but /dev/kvm exists. KVM may be built into the kernel.'
      elif [ -r /proc/modules ]; then
        warn 'x86 vendor KVM module is not listed. Expected kvm_intel or kvm_amd.'
      else
        warn '/proc/modules is not readable. Skipping x86 vendor KVM module check.'
      fi
      ;;
    aarch64 | arm64)
      info 'Skipping x86 vendor KVM module check on ARM.'
      ;;
    *)
      info "Skipping x86 vendor KVM module check for architecture: ${machine_arch}"
      ;;
  esac
}

print_summary() {
  printf '\n%s\n' 'Summary'
  printf '%s\n' '-------'
  printf 'failures=%s warnings=%s\n' "$failures" "$warnings"

  if [ "$failures" -eq 0 ]; then
    printf '%s\n' 'Result: KVM host checks passed.'
    exit 0
  fi

  printf '%s\n' 'Result: KVM host checks failed.'
  exit 1
}

main() {
  print_header
  check_platform
  check_cpu_virtualization
  check_kvm_device
  check_user_access
  check_kvm_module
  print_summary
}

main "$@"
