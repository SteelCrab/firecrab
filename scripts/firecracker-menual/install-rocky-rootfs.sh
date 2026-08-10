#!/usr/bin/env bash
# Build a Firecracker-ready Rocky Linux 9.8 rootfs from official Rocky Container Base tarball.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)

artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
build_dir="${repo_dir}/build/rocky-rootfs"
rocky_release='9.8'
rootfs_size='2G'
rootfs_hostname='firecrab'
extract_vmlinux="${script_dir}/extract-vmlinux"

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

has_command() { command -v "$1" >/dev/null 2>&1; }
require_command() {
  if ! has_command "$1"; then
    fail "Required command not found: $1"
  fi
}

abs_dir() {
  mkdir -p "$1"
  cd "$1" && pwd -P
}

case "${M2IMAGE_ARCH:-$(uname -m 2>/dev/null || printf unknown)}" in
  x86_64|amd64)
    rocky_arch='x86_64'
    kernel_image_name="vmlinux-rocky-${rocky_release}-x86_64"
    ;;
  aarch64|arm64)
    rocky_arch='aarch64'
    kernel_image_name="Image-rocky-${rocky_release}-aarch64"
    ;;
  *)
    fail 'Unsupported architecture. Rocky Linux 9.8 supports x86_64 and aarch64.'
    ;;
esac

initrd_image_name="initramfs-rocky-${rocky_release}-${rocky_arch}"
rootfs_image_name="rocky-rootfs-${rocky_release}-${rocky_arch}.ext4"

resolve_ssh_public_key() {
  local candidate=''
  local sudo_home=''

  if [ -n "${SUDO_UID:-}" ] && has_command getent; then
    sudo_home=$(getent passwd "$SUDO_UID" | cut -d: -f6 || true)
    if [ -n "$sudo_home" ]; then
      candidate="${sudo_home}/.ssh/id_ed25519.pub"
    fi
  fi
  if [ -z "$candidate" ] || [ ! -f "$candidate" ]; then
    candidate="${HOME:-}/.ssh/id_ed25519.pub"
  fi

  if [ -n "$candidate" ] && [ -f "$candidate" ]; then
    printf '%s\n' "$candidate"
  fi
}

configure_rocky_rootfs() {
  local staging=$1

  cat >"${staging}/etc/hostname" <<EOF_HOSTNAME
${rootfs_hostname}
EOF_HOSTNAME

  cat >"${staging}/etc/hosts" <<EOF_HOSTS
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF_HOSTS

  cat >"${staging}/etc/fstab" <<'EOF_FSTAB'
/dev/vda / ext4 defaults 0 1
EOF_FSTAB

  cat >"${staging}/etc/resolv.conf" <<'EOF_RESOLV'
nameserver 172.30.0.1
EOF_RESOLV

  mkdir -p "${staging}/etc/sysconfig/network-scripts"
  cat >"${staging}/etc/sysconfig/network-scripts/ifcfg-eth0" <<'EOF_IFCFG'
DEVICE=eth0
BOOTPROTO=dhcp
ONBOOT=yes
TYPE=Ethernet
EOF_IFCFG

  mkdir -p "${staging}/etc/init.d"
  cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF_SENTINEL'
#!/bin/bash

ipv4=""
for _ in $(seq 1 10); do
    ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
    [ -n "$ipv4" ] && break
    sleep 1
done
if [ -z "$ipv4" ]; then
    echo "FIRECRAB_NETWORK_FAILED no-ipv4-address" >/dev/console
elif getent hosts example.com >/dev/null 2>&1; then
    echo "FIRECRAB_NETWORK_READY $ipv4" >/dev/console
else
    echo "FIRECRAB_NETWORK_FAILED dns-unreachable" >/dev/console
fi
EOF_SENTINEL
  chmod 0755 "${staging}/etc/init.d/firecrab-network-ready"

  if [ -d "${staging}/etc/systemd/system" ]; then
    mkdir -p "${staging}/etc/systemd/system/multi-user.target.wants"
    cat >"${staging}/etc/systemd/system/firecrab-network-ready.service" <<'EOF_SERVICE'
[Unit]
Description=Firecrab Network Readiness Sentinel
After=network-online.target NetworkManager.service
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/etc/init.d/firecrab-network-ready
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF_SERVICE
    ln -sf /etc/systemd/system/firecrab-network-ready.service \
      "${staging}/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"

    mkdir -p "${staging}/etc/systemd/system/getty.target.wants"
    cat >"${staging}/etc/systemd/system/getty@ttyS0.service" <<'EOF_GETTY'
[Unit]
Description=Serial Getty on ttyS0
Documentation=man:agetty(8)
After=rc-local.service

[Service]
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 vt100
Type=idle
Restart=always
UtmpIdentifier=ttyS0
TTYPath=/dev/ttyS0

[Install]
WantedBy=getty.target
EOF_GETTY
    ln -sf /etc/systemd/system/getty@ttyS0.service \
      "${staging}/etc/systemd/system/getty.target.wants/getty@ttyS0.service"
  fi

  local ssh_public_key
  ssh_public_key=$(resolve_ssh_public_key)
  if [ -n "$ssh_public_key" ] && [ -f "$ssh_public_key" ]; then
    mkdir -p -m 0700 "${staging}/root/.ssh"
    cp "$ssh_public_key" "${staging}/root/.ssh/authorized_keys"
    chmod 0600 "${staging}/root/.ssh/authorized_keys"
  fi
}

extract_rocky_kernel() {
  local staging=$1
  mkdir -p "$kernel_artifact_dir"

  local vmlinuz_src=""
  for candidate in $(find "${staging}/boot" -maxdepth 1 -name 'vmlinuz*' 2>/dev/null); do
    if [ -f "$candidate" ]; then
      vmlinuz_src="$candidate"
      break
    fi
  done

  if [ -n "$vmlinuz_src" ]; then
    local kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
    if [ "$rocky_arch" = aarch64 ]; then
      info "preserving Rocky ARM64 PE kernel Image: ${vmlinuz_src}"
      cp "$vmlinuz_src" "$kernel_image_path"
    else
      info "extracting Rocky ELF vmlinux kernel from: ${vmlinuz_src}"
      if [ -x "$extract_vmlinux" ]; then
        "$extract_vmlinux" "$vmlinuz_src" >"$kernel_image_path" || cp "$vmlinuz_src" "$kernel_image_path"
      else
        cp "$vmlinuz_src" "$kernel_image_path"
      fi
    fi
    chmod 0644 "$kernel_image_path"
  fi

  local initramfs_src=""
  for candidate in $(find "${staging}/boot" -maxdepth 1 -name 'initramfs*' 2>/dev/null); do
    if [ -f "$candidate" ]; then
      initramfs_src="$candidate"
      break
    fi
  done
  if [ -n "$initramfs_src" ]; then
    local initrd_image_path="${kernel_artifact_dir}/${initrd_image_name}"
    cp "$initramfs_src" "$initrd_image_path"
    chmod 0644 "$initrd_image_path"
  fi
}

main() {
  require_command awk
  require_command cp
  require_command curl
  require_command grep
  require_command mkdir
  require_command mkfs.ext4
  require_command mv
  require_command sha256sum
  require_command tar
  require_command uname

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")

  info "building Rocky Linux ${rocky_release} (${rocky_arch}) rootfs from official Rocky Container Base tarball"

  download_dir="${build_dir}/downloads"
  mkdir -p "$download_dir"
  archive_name="Rocky-9-Container-Base.latest.${rocky_arch}.tar.xz"
  archive_path="${download_dir}/${archive_name}"

  if [ -f "$archive_path" ]; then
    info "reusing Rocky Container Base archive: ${archive_path}"
  else
    archive_url="https://download.rockylinux.org/pub/rocky/9/images/${rocky_arch}/${archive_name}"
    info "downloading Rocky Container Base archive: ${archive_url}"
    if ! curl -fsSL "$archive_url" -o "${archive_path}.tmp"; then
      archive_url="https://download.rockylinux.org/pub/rocky/9/images/${rocky_arch}/Rocky-9-Container-Base-9.8-latest.${rocky_arch}.tar.xz"
      info "retrying with versioned URL: ${archive_url}"
      if ! curl -fsSL "$archive_url" -o "${archive_path}.tmp"; then
        rm -f "${archive_path}.tmp"
        fail "Could not download Rocky Container Base archive."
      fi
    fi
    mv "${archive_path}.tmp" "$archive_path"
  fi

  mount_dir="${build_dir}/mnt"
  rm -rf "$mount_dir"
  mkdir -p "$mount_dir"

  info 'extracting Rocky Container Base into staging root'
  tar --numeric-owner -xpf "$archive_path" -C "$mount_dir"

  configure_rocky_rootfs "$mount_dir"
  extract_rocky_kernel "$mount_dir"

  rootfs_image="${artifact_dir}/${rootfs_image_name}"
  rootfs_link="${artifact_dir}/rocky-rootfs.ext4"

  info "creating Rocky rootfs image: ${rootfs_image}"
  mkfs.ext4 -F -L rootfs -d "$mount_dir" "$rootfs_image" >/dev/null

  ln -sfn "$(basename "$rootfs_image")" "$rootfs_link"
  info "Rocky rootfs image created: ${rootfs_image}"
}

main "$@"
