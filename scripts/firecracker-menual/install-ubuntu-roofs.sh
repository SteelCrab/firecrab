#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)
script_path="${script_dir}/$(basename -- "$0")"

ubuntu_base_url='https://cdimage.ubuntu.com/ubuntu-base/releases'
ubuntu_series_setting=${M2IMAGE_DISTRO_SERIES:-26.04}
artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
kernel_image_name=''
extract_vmlinux="${script_dir}/extract-vmlinux"
extract_arm64_image="${script_dir}/extract-arm64-image"
build_dir="${repo_dir}/build/ubuntu-rootfs"
rootfs_image=''
rootfs_link=''
rootfs_size='2G'
rootfs_hostname='firecrab'
m2image_alias=${M2IMAGE_ALIAS:-}
sbom_output=${M2IMAGE_SBOM_OUTPUT:-}
sbom_generator="${repo_dir}/scripts/m2image_sbom.py"

# linux-image-virtual: Ubuntu's own officially-maintained kernel package
# (public-docs/images.md) — replaces the self-built vanilla kernel every
# template used to share, so security patches and driver support follow
# Ubuntu's own release cadence instead of this project having to track
# kernel.org itself.
#
# The *virtual* meta-package, not *generic*, though both resolve to the
# identical binary (both Depend on linux-image-<abi>-generic, so the kernel,
# its cadence and its module set are unchanged). The difference is one line
# of packaging: linux-image-generic additionally carries
# "Depends: linux-firmware | linux-firmware-minimal", and apt always takes
# the first alternative — which is a hard dependency, so
# --no-install-recommends cannot decline it. That dragged in every
# linux-firmware-* subpackage: Qualcomm/Intel/Broadcom/Marvell/Realtek
# wireless, NVIDIA/AMD/Intel graphics, Mellanox/Netronome/QLogic NICs.
# Measured inside the built image with debugfs, that was 656 MB across 5233
# files — 53% of the rootfs's 1.24 GiB of content — for hardware a
# Firecracker microVM cannot have. None of it is reachable at boot either:
# this template ships no initrd (`"initrd": null` in packaging/m2images.json)
# and mounts root=/dev/vda straight from the kernel's built-in
# CONFIG_VIRTIO_BLK/VIRTIO_MMIO/EXT4_FS, so firmware and modules play no part
# in bringing the guest up.
rootfs_boot_packages='systemd systemd-sysv systemd-resolved udev kmod util-linux linux-image-virtual'
rootfs_packages="${rootfs_boot_packages} iproute2 iputils-ping net-tools dnsutils curl ca-certificates procps openssh-server"

mount_dir=''
chroot_mounts=''

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

abs_dir() {
  path=$1

  mkdir -p "$path"
  cd "$path" && pwd -P
}

abs_file() {
  path=$1

  case "$path" in
    /*)
      printf '%s\n' "$path"
      ;;
    *)
      printf '%s/%s\n' "$(pwd -P)" "$path"
      ;;
  esac
}

link_target_for() {
  target=$1
  link_path=$2
  target_dir=${target%/*}
  link_dir=${link_path%/*}

  if [ "$target_dir" = "$link_dir" ]; then
    printf '%s\n' "${target##*/}"
  else
    printf '%s\n' "$target"
  fi
}

update_symlink() {
  target=$1
  link_path=$2

  if [ "$target" = "$link_path" ]; then
    return
  fi

  if [ -d "$link_path" ] && [ ! -L "$link_path" ]; then
    fail "Cannot replace directory with symlink: ${link_path}"
  fi

  link_target=$(link_target_for "$target" "$link_path")
  ln -sfn "$link_target" "$link_path"
  printf '%s\n' "$link_target"
}

write_root_file() {
  target=$1
  cat >"$target"
}

install_authorized_ssh_key() {
  local key_source=''
  local key_home=''

  if [ -n "${SUDO_USER:-}" ] && [ -n "${SUDO_UID:-}" ]; then
    key_home=$(getent passwd "$SUDO_UID" | cut -d: -f6)
  elif [ -n "${HOME:-}" ]; then
    key_home=$HOME
  fi

  if [ -n "$key_home" ]; then
    for key_source in \
      "$key_home/.ssh/id_ed25519.pub" \
      "$key_home/.ssh/id_ecdsa.pub" \
      "$key_home/.ssh/id_rsa.pub"; do
      [ -s "$key_source" ] && break
      key_source=''
    done
  fi

  if [ -z "$key_source" ]; then
    info 'no host SSH public key found; building with serial-console access only'
    return
  fi

  install -d -m 0700 "${mount_dir}/root/.ssh"
  install -m 0600 "$key_source" "${mount_dir}/root/.ssh/authorized_keys"
}

cleanup() {
  cleanup_chroot_mounts
}

trap cleanup EXIT

detect_ubuntu_arch() {
  case "${M2IMAGE_ARCH:-${UBUNTU_ARCH:-$(uname -m 2>/dev/null || printf 'unknown')}}" in
    x86_64 | amd64)
      printf '%s\n' 'amd64'
      ;;
    aarch64 | arm64)
      printf '%s\n' 'arm64'
      ;;
    *)
      fail 'Unsupported architecture. Ubuntu Base rootfs creation supports amd64 and arm64.'
      ;;
  esac
}

resolve_ubuntu_base_archive() {
  series=$1
  release_url="${ubuntu_base_url}/${series}/release"
  index_html="${build_dir}/ubuntu-base-index.html"

  mkdir -p "$build_dir"
  if ! fetch_url "${release_url}/" "$index_html"; then
    fail "Could not download Ubuntu Base release index: ${release_url}/"
  fi

  archive_name=$(find_ubuntu_base_archive "$index_html")

  if [ -z "$archive_name" ]; then
    fail "Could not find an Ubuntu Base archive for ${series}/${ubuntu_arch}."
  fi

  printf '%s/%s\n' "$release_url" "$archive_name"
}

find_ubuntu_base_archive() {
  index_html=$1

  grep -Eo "ubuntu-base-[0-9.]+-base-${ubuntu_arch}\\.tar\\.gz" "$index_html" |
    sort -V |
    tail -n 1 ||
    true
}

fetch_url() {
  url=$1
  output_path=$2
  output_tmp="${output_path}.tmp"

  if curl -fsSL "$url" -o "$output_tmp" 2>/dev/null; then
    mv "$output_tmp" "$output_path"
    return 0
  fi

  rm -f "$output_tmp"
  [ -f "$output_path" ]
}

resolve_ubuntu_series() {
  if [ "$ubuntu_series_setting" != 'latest' ]; then
    printf '%s\n' "$ubuntu_series_setting"
    return
  fi

  releases_index="${build_dir}/ubuntu-base-releases-index.html"

  mkdir -p "$build_dir"
  if ! fetch_url "${ubuntu_base_url}/" "$releases_index"; then
    fail "Could not download Ubuntu Base releases index: ${ubuntu_base_url}/"
  fi

  release_candidates=$(
    grep -Eo '[0-9]{2}\.[0-9]{2}(\.[0-9]+)?/' "$releases_index" |
      sed 's:/$::' |
      sort -Vu |
      sort -Vr ||
      true
  )

  if [ -z "$release_candidates" ]; then
    fail 'Could not resolve the latest Ubuntu Base release.'
  fi

  for series in $release_candidates; do
    release_index="${build_dir}/ubuntu-base-${series}-index.html"

    if ! fetch_url "${ubuntu_base_url}/${series}/release/" "$release_index"; then
      continue
    fi

    if [ -n "$(find_ubuntu_base_archive "$release_index")" ]; then
      printf '%s\n' "$series"
      return
    fi
  done

  fail "Could not find any Ubuntu Base archive for ${ubuntu_arch}."
}

verify_checksum() {
  checksum_file="${download_dir}/SHA256SUMS"
  checksum_line=$(grep -E "[ *]${archive_name}$" "$checksum_file" || true)

  if [ -z "$checksum_line" ]; then
    fail "Checksum entry not found for archive: ${archive_name}"
  fi

  printf '%s\n' "$checksum_line" | (cd "$download_dir" && sha256sum -c -)
}

fetch_checksum() {
  checksum_file="${download_dir}/SHA256SUMS"
  checksum_tmp="${checksum_file}.tmp"
  checksum_url="${ubuntu_base_url}/${ubuntu_series}/release/SHA256SUMS"

  if curl -fsSL "$checksum_url" -o "$checksum_tmp" 2>/dev/null; then
    mv "$checksum_tmp" "$checksum_file"
    return
  fi

  rm -f "$checksum_tmp"
  if [ -f "$checksum_file" ]; then
    info "reusing cached checksum file: ${checksum_file}"
    return
  fi

  fail "Could not download Ubuntu Base checksums: ${checksum_url}"
}

download_archive() {
  archive_tmp="${archive_path}.tmp"

  if [ -f "$archive_path" ]; then
    info "reusing Ubuntu Base archive: ${archive_path}"
    return
  fi

  info "downloading Ubuntu Base archive: ${archive_url}"
  if ! curl -fsSL "$archive_url" -o "$archive_tmp" 2>/dev/null; then
    rm -f "$archive_tmp"
    fail "Could not download Ubuntu Base archive: ${archive_url}"
  fi

  mv "$archive_tmp" "$archive_path"
}

check_rootfs_output() {
  if [ -d "$rootfs_image" ] && [ ! -L "$rootfs_image" ]; then
    fail "Rootfs image path exists as a directory: ${rootfs_image}"
  fi

  if [ "$rootfs_link" != "$rootfs_image" ] &&
    [ -d "$rootfs_link" ] &&
    [ ! -L "$rootfs_link" ]; then
    fail "Rootfs symlink path exists as a directory: ${rootfs_link}"
  fi
}

print_rootfs_outputs() {
  info "Ubuntu rootfs image: ${rootfs_image}"
  if [ "$rootfs_link" != "$rootfs_image" ]; then
    info "Ubuntu rootfs symlink: ${rootfs_link} -> ${rootfs_link_target}"
  fi
}

restore_output_ownership() {
  if [ -z "${SUDO_UID:-}" ] || [ -z "${SUDO_GID:-}" ]; then
    return
  fi

  chown "${SUDO_UID}:${SUDO_GID}" "$rootfs_image"
  chmod u+rw,go+r "$rootfs_image"

  if [ "$rootfs_link" != "$rootfs_image" ] && [ -L "$rootfs_link" ]; then
    chown -h "${SUDO_UID}:${SUDO_GID}" "$rootfs_link" 2>/dev/null || true
  fi

  chown "${SUDO_UID}:${SUDO_GID}" "${kernel_artifact_dir}/${kernel_image_name}"
}

cleanup_chroot_mounts() {
  local target

  for target in $chroot_mounts; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done

  chroot_mounts=''
}

mount_chroot_fs() {
  cp /etc/resolv.conf "${mount_dir}/etc/resolv.conf"

  mount -t proc proc "${mount_dir}/proc"
  chroot_mounts="${mount_dir}/proc ${chroot_mounts}"

  mount --rbind /sys "${mount_dir}/sys"
  mount --make-rslave "${mount_dir}/sys"
  chroot_mounts="${mount_dir}/sys ${chroot_mounts}"

  mount --rbind /dev "${mount_dir}/dev"
  mount --make-rslave "${mount_dir}/dev"
  chroot_mounts="${mount_dir}/dev ${chroot_mounts}"
}

prepare_rootfs_image() {
  mkdir -p "$(dirname "$rootfs_image")"
  if [ "$rootfs_link" != "$rootfs_image" ]; then
    mkdir -p "$(dirname "$rootfs_link")"
  fi

  if [ -e "$rootfs_image" ] || [ -L "$rootfs_image" ]; then
    rm -f "$rootfs_image"
  fi

  if [ "$rootfs_link" != "$rootfs_image" ] && { [ -e "$rootfs_link" ] || [ -L "$rootfs_link" ]; }; then
    rm -f "$rootfs_link"
  fi

  # Create a raw image that Firecracker can expose as /dev/vda.
  truncate -s "$rootfs_size" "$rootfs_image"
}

configure_rootfs() {
  # Add the minimal host identity and mount table expected by the Ubuntu userspace.
  write_root_file "${mount_dir}/etc/hostname" <<EOF
${rootfs_hostname}
EOF

  write_root_file "${mount_dir}/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF

  write_root_file "${mount_dir}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF

  write_root_file "${mount_dir}/etc/machine-id" <<'EOF'
EOF

  install_authorized_ssh_key

  # Configure the Firecracker guest network interface via DHCP — the host's
  # net-helper/dnsmasq hands out the IPAM-allocated address by MAC
  # reservation (public-docs/networking.md), so the guest never
  # hardcodes an address itself.
  install -d -m 0755 "${mount_dir}/etc/systemd/network"
  write_root_file "${mount_dir}/etc/systemd/network/10-eth0.network" <<'EOF'
[Match]
Name=eth0

[Network]
DHCP=yes
EOF

  install -d -m 0755 "${mount_dir}/etc/systemd/system/multi-user.target.wants"
  ln -sf /lib/systemd/system/systemd-networkd.service \
    "${mount_dir}/etc/systemd/system/multi-user.target.wants/systemd-networkd.service"
  install -d -m 0755 "${mount_dir}/etc/systemd/system/sockets.target.wants"
  ln -sf /lib/systemd/system/systemd-networkd.socket \
    "${mount_dir}/etc/systemd/system/sockets.target.wants/systemd-networkd.socket"

  install_network_ready_sentinel

  # Enable a serial console getty so Firecracker console output reaches ttyS0.
  install -d -m 0755 "${mount_dir}/etc/systemd/system/getty.target.wants"
  ln -sf /lib/systemd/system/serial-getty@.service \
    "${mount_dir}/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"
  install -d -m 0755 "${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d"
  write_root_file "${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

  # Ensure standard runtime directories exist with expected permissions.
  install -d -m 0755 "${mount_dir}/dev" "${mount_dir}/proc" "${mount_dir}/sys" "${mount_dir}/run"
  install -d -m 1777 "${mount_dir}/tmp"
}

# firecrab-net-helper's dnsmasq answers DNS on the bridge gateway itself
# (172.30.0.1) for every guest on the VPC subnet, so the guest points at
# that instead of a public resolver — matches how a NAT router's own
# address is typically handed out as the DNS server.
configure_guest_dns() {
  write_root_file "${mount_dir}/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF
}

# Prints a fixed sentinel line to the serial console (ttyS0, which is
# Firecracker's captured stdout) once DHCP + DNS are confirmed working —
# the signal firecrab-api's start pipeline waits on in place of a guest
# agent event (public-docs/networking.md; guest agent/vsock is
# out of this project's competition scope).
install_network_ready_sentinel() {
  install -d -m 0755 "${mount_dir}/usr/local/sbin"
  write_root_file "${mount_dir}/usr/local/sbin/firecrab-network-ready.sh" <<'EOF'
#!/bin/sh
set -eu
# network-online only orders unit *starts* — DHCP and DNS may still be
# unfinished. Poll for IPv4, repair a dangling systemd-resolved stub, then
# accept getent or dig@gateway (host dnsmasq).
ipv4=""
for _ in $(seq 1 15); do
  ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
    awk '$2 != "lo" { split($4, a, "/"); print a[1]; exit }')
  if [ -n "$ipv4" ]; then
    break
  fi
  sleep 1
done
if [ -z "$ipv4" ]; then
  echo "FIRECRAB_NETWORK_FAILED no-ipv4-address"
  exit 0
fi
gw=$(ip -4 route show default 2>/dev/null | awk '{print $3; exit}')
if [ -n "$gw" ] && [ ! -e /run/systemd/resolve/stub-resolv.conf ]; then
  if [ -L /etc/resolv.conf ] || [ ! -s /etc/resolv.conf ]; then
    rm -f /etc/resolv.conf
    printf 'nameserver %s\n' "$gw" > /etc/resolv.conf
  fi
fi
dns_ok() {
  getent hosts example.com >/dev/null 2>&1 && return 0
  if [ -n "$gw" ] && command -v dig >/dev/null 2>&1; then
    ans=$(dig +short +time=2 +tries=1 @"$gw" example.com A 2>/dev/null || true)
    [ -n "$ans" ] && return 0
  fi
  return 1
}
for _ in $(seq 1 15); do
  if dns_ok; then
    echo "FIRECRAB_NETWORK_READY $ipv4"
    exit 0
  fi
  sleep 1
done
echo "FIRECRAB_NETWORK_FAILED dns-unreachable"
EOF
  chmod 0755 "${mount_dir}/usr/local/sbin/firecrab-network-ready.sh"

  write_root_file "${mount_dir}/etc/systemd/system/firecrab-network-ready.service" <<'EOF'
[Unit]
Description=Firecrab network readiness sentinel
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
StandardOutput=tty
TTYPath=/dev/console
ExecStart=/usr/local/sbin/firecrab-network-ready.sh

[Install]
WantedBy=multi-user.target
EOF
  install -d -m 0755 "${mount_dir}/etc/systemd/system/multi-user.target.wants"
  ln -sf /etc/systemd/system/firecrab-network-ready.service \
    "${mount_dir}/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"
}

install_rootfs_packages() {
  if [ -z "$rootfs_packages" ]; then
    return
  fi

  info "installing rootfs packages: ${rootfs_packages}"
  mount_chroot_fs
  chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get update
  # shellcheck disable=SC2086
  chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $rootfs_packages
  chroot "$mount_dir" apt-get clean
  rm -rf "${mount_dir}/var/lib/apt/lists/"*
  cleanup_chroot_mounts
}

# True when the file is a raw Linux/arm64 Image: ARM64_IMAGE_MAGIC ("ARMd")
# at offset 0x38, the field Firecracker's loader checks before it jumps into
# the image. Read, never written — a wrapped kernel (a UKI or an EFI zboot
# image) with the magic stamped on top passes the loader and then dies
# silently in the guest instead of failing here.
is_arm64_image() {
  [ -s "$1" ] && [ "$(od -An -tx1 -j56 -N4 "$1" | tr -d ' \n')" = '41524d64' ]
}

# Pulls the distro kernel out of the rootfs. x86_64 is converted to the
# uncompressed ELF vmlinux Firecracker expects. ARM64 is unwrapped to the raw
# Linux/arm64 Image, the only format Firecracker boots on that architecture:
# Ubuntu's arm64 vmlinuz is a UKI whose .linux section holds an EFI zboot
# image, so neither the file itself nor its first payload is bootable.
extract_kernel() {
  vmlinuz_path=$(find "${mount_dir}/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -n 1)
  if [ -z "$vmlinuz_path" ]; then
    fail "linux-image-virtual did not install a vmlinuz under ${mount_dir}/boot"
  fi
  mkdir -p "$kernel_artifact_dir"
  kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
  kernel_image_tmp="${kernel_image_path}.tmp"
  if [ "$ubuntu_arch" = arm64 ]; then
    info "extracting raw ARM64 Image from: ${vmlinuz_path}"
    if ! "$extract_arm64_image" "$vmlinuz_path" >"$kernel_image_tmp" \
      || ! is_arm64_image "$kernel_image_tmp"; then
      rm -f "$kernel_image_tmp"
      fail "extract-arm64-image could not extract a raw ARM64 Image from ${vmlinuz_path}"
    fi
  else
    info "extracting ELF vmlinux from: ${vmlinuz_path}"
    if ! "$extract_vmlinux" "$vmlinuz_path" >"$kernel_image_tmp"; then
      rm -f "$kernel_image_tmp"
      fail "extract-vmlinux could not extract an ELF vmlinux from ${vmlinuz_path}"
    fi
    if ! file "$kernel_image_tmp" | grep -q 'ELF'; then
      rm -f "$kernel_image_tmp"
      fail "extracted kernel is not an ELF image: ${vmlinuz_path}"
    fi
  fi
  chmod 0644 "$kernel_image_tmp"
  mv "$kernel_image_tmp" "$kernel_image_path"
  info "Ubuntu kernel image: ${kernel_image_path}"
}

verify_rootfs_content() {
  if [ ! -e "${mount_dir}/etc/os-release" ]; then
    fail "Ubuntu Base extraction did not create /etc/os-release in ${rootfs_image}"
  fi

  if [ ! -e "${mount_dir}/bin/sh" ]; then
    fail "Ubuntu Base extraction did not create /bin/sh in ${rootfs_image}"
  fi

  if [ ! -e "${mount_dir}/sbin/init" ]; then
    fail "Rootfs did not install /sbin/init. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/sbin/reboot" ] && [ ! -e "${mount_dir}/usr/sbin/reboot" ]; then
    fail "Rootfs did not install reboot. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/sbin/agetty" ] && [ ! -e "${mount_dir}/usr/sbin/agetty" ]; then
    fail "Rootfs did not install agetty. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/usr/sbin/sshd" ]; then
    fail "Rootfs did not install sshd. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/etc/systemd/network/10-eth0.network" ]; then
    fail 'Rootfs did not create /etc/systemd/network/10-eth0.network.'
  fi

  networkd_service_link="${mount_dir}/etc/systemd/system/multi-user.target.wants/systemd-networkd.service"
  if [ ! -L "${networkd_service_link}" ]; then
    fail 'Rootfs did not enable systemd-networkd.service.'
  fi
  networkd_service_target=$(readlink "${networkd_service_link}")
  if [ "${networkd_service_target}" != '/lib/systemd/system/systemd-networkd.service' ]; then
    fail 'Rootfs did not enable systemd-networkd.service.'
  fi
  if [ ! -e "${mount_dir}${networkd_service_target}" ]; then
    fail 'Rootfs did not enable systemd-networkd.service.'
  fi

  if [ ! -x "${mount_dir}/usr/local/sbin/firecrab-network-ready.sh" ]; then
    fail 'Rootfs did not install an executable firecrab-network-ready.sh.'
  fi

  if [ ! -e "${mount_dir}/etc/systemd/system/firecrab-network-ready.service" ]; then
    fail 'Rootfs did not install firecrab-network-ready.service.'
  fi

  if [ ! -L "${mount_dir}/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service" ]; then
    fail 'Rootfs did not enable firecrab-network-ready.service.'
  fi

  if [ ! -e "${mount_dir}/bin/udevadm" ] && [ ! -e "${mount_dir}/usr/bin/udevadm" ] &&
    [ ! -e "${mount_dir}/sbin/udevadm" ] && [ ! -e "${mount_dir}/usr/sbin/udevadm" ]; then
    fail "Rootfs did not install udevadm. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/bin/kmod" ] && [ ! -e "${mount_dir}/usr/bin/kmod" ]; then
    fail "Rootfs did not install kmod. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/sbin/ip" ] && [ ! -e "${mount_dir}/usr/sbin/ip" ] &&
    [ ! -e "${mount_dir}/bin/ip" ] && [ ! -e "${mount_dir}/usr/bin/ip" ]; then
    fail "Rootfs did not install ip. Check packages: ${rootfs_packages}"
  fi

  if [ ! -e "${mount_dir}/bin/ping" ] && [ ! -e "${mount_dir}/usr/bin/ping" ]; then
    fail "Rootfs did not install ping. Check packages: ${rootfs_packages}"
  fi

  kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
  if [ ! -s "$kernel_image_path" ]; then
    fail "extract_kernel did not produce a kernel image: ${kernel_image_path}"
  fi
  case "$ubuntu_arch" in
    amd64)
      file "$kernel_image_path" | grep -q 'ELF' \
        || fail "Extracted kernel image is not ELF: ${kernel_image_path}"
      ;;
    arm64)
      is_arm64_image "$kernel_image_path" \
        || fail "ARM64 kernel is not a raw Image: ${kernel_image_path}"
      ;;
  esac
}

main() {
  if [ "$#" -ne 0 ]; then
    fail 'install-ubuntu-roofs.sh does not accept arguments.'
  fi

  # Check local tools required to download, verify, stage, and create the rootfs image.
  require_command chmod
  require_command chown
  require_command chroot
  require_command cp
  require_command curl
  require_command file
  require_command find
  require_command grep
  require_command install
  require_command ln
  require_command mkdir
  require_command mkfs.ext4
  require_command mount
  require_command mv
  require_command rm
  require_command sed
  require_command sha256sum
  require_command sort
  require_command tail
  require_command tar
  require_command truncate
  require_command uname
  require_command umount
  require_command od
  if [ ! -x "$extract_vmlinux" ]; then
    fail "extract-vmlinux helper not found or not executable: ${extract_vmlinux}"
  fi
  if [ ! -x "$extract_arm64_image" ]; then
    fail "extract-arm64-image helper not found or not executable: ${extract_arm64_image}"
  fi

  if [ "$(id -u)" -ne 0 ]; then
    require_command sudo
    exec sudo env \
      "M2IMAGE_ARCH=${M2IMAGE_ARCH:-}" \
      "M2IMAGE_DISTRO_SERIES=${ubuntu_series_setting}" \
      "M2IMAGE_DISTRO_VERSION=${M2IMAGE_DISTRO_VERSION:-}" \
      "M2IMAGE_ALIAS=${m2image_alias}" \
      "M2IMAGE_SBOM_OUTPUT=${sbom_output}" \
      "$script_path"
  fi

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  ubuntu_arch=$(detect_ubuntu_arch)
  ubuntu_series=$(resolve_ubuntu_series)
  case "$ubuntu_arch" in
    amd64) kernel_image_name="vmlinux-ubuntu-${ubuntu_series}-x86_64" ;;
    arm64) kernel_image_name="Image-ubuntu-${ubuntu_series}-aarch64" ;;
  esac
  if [ -z "$rootfs_image" ]; then
    rootfs_image="${artifact_dir}/ubuntu-rootfs-${ubuntu_series}-${ubuntu_arch}.ext4"
  else
    rootfs_image=$(abs_file "$rootfs_image")
  fi
  if [ -z "$rootfs_link" ]; then
    rootfs_link="${artifact_dir}/ubuntu-rootfs.ext4"
  else
    rootfs_link=$(abs_file "$rootfs_link")
  fi

  archive_url=$(resolve_ubuntu_base_archive "$ubuntu_series")
  archive_name=${archive_url##*/}
  download_dir="${build_dir}/downloads"
  archive_path="${download_dir}/${archive_name}"
  mount_dir="${build_dir}/mnt"

  info "Ubuntu series: ${ubuntu_series}"
  info "Ubuntu architecture: ${ubuntu_arch}"
  info "rootfs versioned image: ${rootfs_image}"
  info "rootfs symlink: ${rootfs_link}"
  info "rootfs size: ${rootfs_size}"
  if [ -n "$rootfs_packages" ]; then
    info "rootfs packages: ${rootfs_packages}"
  fi
  check_rootfs_output

  # Download the Ubuntu Base rootfs tarball and verify it with the official checksum file.
  mkdir -p "$download_dir"
  download_archive
  fetch_checksum
  verify_checksum

  # Extract Ubuntu Base into a staging directory before building the ext4 image.
  prepare_rootfs_image
  rm -rf "$mount_dir"
  mkdir -p "$mount_dir"
  info 'extracting Ubuntu Base into rootfs image'
  tar --numeric-owner -xpf "$archive_path" -C "$mount_dir"
  configure_rootfs
  install_rootfs_packages
  extract_kernel
  configure_guest_dns
  verify_rootfs_content

  if [ -n "$sbom_output" ]; then
    [ -n "$m2image_alias" ] || fail 'M2IMAGE_ALIAS is required with M2IMAGE_SBOM_OUTPUT'
    require_command python3
    [ -f "$mount_dir/var/lib/dpkg/status" ] \
      || fail 'Ubuntu dpkg status database missing from built staging root'
    case "$ubuntu_arch" in
      amd64) sbom_arch=x86_64 ;;
      arm64) sbom_arch=aarch64 ;;
    esac
    python3 "$sbom_generator" \
      --format dpkg --distribution ubuntu \
      --image-alias "$m2image_alias" --image-version "$ubuntu_series" \
      --architecture "$sbom_arch" \
      --package-db "$mount_dir/var/lib/dpkg/status" --output "$sbom_output"
    if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
      chown "${SUDO_UID}:${SUDO_GID}" "$sbom_output"
    fi
  fi

  info "creating Ubuntu rootfs image: ${rootfs_image}"
  # Disable orphan_file when the host's e2fsprogs supports it so images built
  # by 1.47 remain readable on hosts/guests that still ship e2fsprogs 1.46.
  if grep -qw orphan_file /etc/mke2fs.conf 2>/dev/null; then
    mkfs.ext4 -F -O '^orphan_file' -L rootfs -d "$mount_dir" "$rootfs_image" >/dev/null
  else
    mkfs.ext4 -F -L rootfs -d "$mount_dir" "$rootfs_image" >/dev/null
  fi
  rootfs_link_target=$(update_symlink "$rootfs_image" "$rootfs_link")
  restore_output_ownership

  info "Ubuntu rootfs image created: ${rootfs_image}"
  print_rootfs_outputs
}

main "$@"
