#!/usr/bin/env bash
# Build a Firecracker-ready version-pinned Rocky Linux rootfs with its EL9 kernel.
#
# The result deliberately stays a direct ext4 file rather than Rocky's cloud
# QCOW2 image: firecrab resizes and customizes rootfs files with e2fsprogs.
# The official Rocky Container-Base tarball is unpacked as a temporary chroot;
# Docker and other OCI runtimes are not used.
#
# EL9 x86_64 uses virtio-pci because its kernel leaves CONFIG_VIRTIO_MMIO off.
# Rocky's aarch64 kernel supports Firecracker's normal virtio-mmio transport.
# Both initramfs variants carry their architecture's transport plus
# virtio_blk/net and ext4 for the guest rootfs.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)
script_path="${script_dir}/$(basename -- "$0")"

artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
build_dir="${repo_dir}/build/rocky-rootfs"
rocky_release=${M2IMAGE_DISTRO_VERSION:-9.8}
rocky_repository_base=${ROCKY_REPOSITORY_BASE:-https://download.rockylinux.org/pub/rocky}
rocky_container_build=${ROCKY_CONTAINER_BUILD:-20260525.0}
rootfs_size='2G'
rootfs_hostname='firecrab'
m2image_alias=${M2IMAGE_ALIAS:-}
sbom_output=${M2IMAGE_SBOM_OUTPUT:-}
sbom_generator="${repo_dir}/scripts/m2image_sbom.py"
extract_vmlinux="${script_dir}/extract-vmlinux"
extract_arm64_image="${script_dir}/extract-arm64-image"
container_mounts=()

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
    printf '[FAIL] Unsupported architecture. Rocky Linux supports x86_64 and aarch64.\n' >&2
    exit 1
    ;;
esac
initrd_image_name="initramfs-rocky-${rocky_release}-${rocky_arch}"
rootfs_image_name="rocky-rootfs-${rocky_release}-${rocky_arch}.ext4"
container_name="Rocky-9-Container-Base-${rocky_release}-${rocky_container_build}.${rocky_arch}.oci.tar.xz"
container_url="${rocky_repository_base}/${rocky_release}/images/${rocky_arch}/${container_name}"

# `kernel` provides the matching kernel-core/modules pair. Rocky's generic
# kernel has virtio/ext4 as modules, so dracut below produces a generic initrd
# containing the Firecracker storage/network drivers.
# dnf (+ rpm via deps) must live *inside* the guest: host `dnf --installroot`
# only stages packages and never installs the package manager itself. Without
# it the dashboard package actions (`dnf -y install …`) fail with "command not
# found" on every Rocky VM.
rootfs_packages='kernel dracut systemd systemd-udev NetworkManager iproute iputils bind-utils curl ca-certificates procps-ng openssh-server kmod util-linux dhcp-client e2fsprogs dnf'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "Required command not found: $1"
}

abs_dir() {
  mkdir -p "$1"
  cd "$1" && pwd -P
}

verify_native_architecture() {
  local host_arch=''

  case "$(uname -m 2>/dev/null || printf unknown)" in
    x86_64|amd64) host_arch='x86_64' ;;
    aarch64|arm64) host_arch='aarch64' ;;
    *) fail 'Unsupported build host architecture.' ;;
  esac
  [ "$host_arch" = "$rocky_arch" ] \
    || fail "Host-native Rocky chroot requires a ${rocky_arch} host (current: ${host_arch})."
}

resolve_ssh_public_key() {
  local candidate=${FIRECRAB_SSH_PUBLIC_KEY:-}
  local sudo_home=''

  if [ -n "$candidate" ]; then
    [ -s "$candidate" ] \
      || fail "FIRECRAB_SSH_PUBLIC_KEY is not a readable public key: ${candidate}"
    printf '%s\n' "$candidate"
    return
  fi

  if [ -n "${SUDO_UID:-}" ] && command -v getent >/dev/null 2>&1; then
    sudo_home=$(getent passwd "$SUDO_UID" | cut -d: -f6 || true)
  elif [ -n "${HOME:-}" ]; then
    sudo_home=$HOME
  fi

  if [ -n "$sudo_home" ]; then
    for candidate in \
      "$sudo_home/.ssh/id_ed25519.pub" \
      "$sudo_home/.ssh/id_ecdsa.pub" \
      "$sudo_home/.ssh/id_rsa.pub"; do
      [ ! -s "$candidate" ] || { printf '%s\n' "$candidate"; return; }
    done
  fi

  candidate="${build_dir}/no-authorized-key.pub"
  : >"$candidate"
  info 'no host SSH public key found; building with serial-console access only' >&2
  printf '%s\n' "$candidate"
}

write_configure_script() {
  cat >"$1" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

staging=/work/rootfs
rootfs_size=$1
rootfs_hostname=$2
rootfs_packages=$3
initrd_image_name=$4
rocky_release=$5
rocky_arch=$6
rootfs_image_name=$7
rocky_repository_base=$8

baseos_url="${rocky_repository_base}/${rocky_release}/BaseOS/${rocky_arch}/os/"
appstream_url="${rocky_repository_base}/${rocky_release}/AppStream/${rocky_arch}/os/"

info() { printf '[ROCKY] %s\n' "$*"; }
fail() { printf '[ROCKY:FAIL] %s\n' "$*" >&2; exit 1; }

chroot_mounts=''

cleanup_chroot_mounts() {
  local target
  for target in $chroot_mounts; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done
  chroot_mounts=''
}

mount_chroot_fs() {
  mount -t proc proc "$staging/proc"
  chroot_mounts="$staging/proc"
  mount --rbind /sys "$staging/sys"
  mount --make-rslave "$staging/sys"
  chroot_mounts="$staging/sys $chroot_mounts"
  mount --rbind /dev "$staging/dev"
  mount --make-rslave "$staging/dev"
  chroot_mounts="$staging/dev $chroot_mounts"
  mount --rbind /run "$staging/run"
  mount --make-rslave "$staging/run"
  chroot_mounts="$staging/run $chroot_mounts"
}

trap cleanup_chroot_mounts EXIT

# This exact mount point is supplied by the host builder; removing only its
# previous contents makes repeat runs deterministic without touching anything
# outside the Rocky staging root.
rm -rf /work/rootfs/* /work/rootfs/.[!.]* 2>/dev/null || true
mkdir -p "$staging/etc/pki" "$staging/dev" "$staging/proc" "$staging/sys" "$staging/run"
cp -a /etc/pki/rpm-gpg "$staging/etc/pki/"

dnf_common=(
  --disablerepo='*'
  --enablerepo=baseos,appstream
  --setopt=baseos.mirrorlist=
  --setopt="baseos.baseurl=${baseos_url}"
  --setopt=appstream.mirrorlist=
  --setopt="appstream.baseurl=${appstream_url}"
  --setopt=install_weak_deps=False
  --setopt=keepcache=False
)

# The builder container needs mkfs.ext4. The guest itself receives its own
# e2fsprogs package below, so it can later be resized by firecrab normally.
info 'installing e2fsprogs in the throwaway builder container'
dnf -q -y "${dnf_common[@]}" install e2fsprogs

info "installing Rocky Linux ${rocky_release} guest packages into the staging root"
# EL9 kernel RPM post-processing invokes dracut under the install root. Give
# that chroot the normal pseudo-filesystems before the transaction so its own
# first initramfs pass succeeds; the explicit generic pass below then replaces
# it with Firecracker's driver set.
mount_chroot_fs
# shellcheck disable=SC2086 -- package names are a deliberate whitespace list.
dnf -q -y --installroot="$staging" --releasever="$rocky_release" --setopt=reposdir=/etc/yum.repos.d \
  "${dnf_common[@]}" install $rootfs_packages

# Stock rocky.repo mirrorlists expand $rltype, which this image never sets
# (same Docker BaseOS 404 the build avoids). Pin public baseurls for guest dnf.
cat >"$staging/etc/yum.repos.d/rocky-firecrab.repo" <<EOF_REPOS
[baseos]
name=Rocky Linux \$releasever - BaseOS (firecrab)
baseurl=${rocky_repository_base}/\$releasever/BaseOS/\$basearch/os/
gpgcheck=1
enabled=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-Rocky-9

[appstream]
name=Rocky Linux \$releasever - AppStream (firecrab)
baseurl=${rocky_repository_base}/\$releasever/AppStream/\$basearch/os/
gpgcheck=1
enabled=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-Rocky-9
EOF_REPOS
# Disable stock enabled sections so only the fixed-url repos above are used.
if [ -f "$staging/etc/yum.repos.d/rocky.repo" ]; then
  sed -i 's/^enabled=1/enabled=0/' "$staging/etc/yum.repos.d/rocky.repo"
fi

test -x "$staging/usr/bin/dnf" || test -x "$staging/bin/dnf" || \
  fail 'Rocky rootfs is missing /usr/bin/dnf after package install'

rm -rf "$staging/var/cache/dnf" "$staging/var/log/dnf"* \
  "$staging/var/cache/yum" "$staging/var/log/yum"* 2>/dev/null || true

cat >"$staging/etc/hostname" <<EOF_HOSTNAME
${rootfs_hostname}
EOF_HOSTNAME

cat >"$staging/etc/hosts" <<EOF_HOSTS
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF_HOSTS

cat >"$staging/etc/fstab" <<'EOF_FSTAB'
/dev/vda / ext4 defaults 0 1
EOF_FSTAB

rm -f "$staging/etc/resolv.conf"
cat >"$staging/etc/resolv.conf" <<'EOF_RESOLV'
nameserver 172.30.0.1
EOF_RESOLV

: >"$staging/etc/machine-id"
install -d -m 0755 "$staging/etc/modules-load.d"
cat >"$staging/etc/modules-load.d/firecrab-network.conf" <<'EOF_MODULES'
virtio_net
EOF_MODULES
install -d -m 0755 "$staging/etc/NetworkManager/system-connections"
cat >"$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection" <<'EOF_NETWORK'
[connection]
id=firecrab-ethernet
type=ethernet
autoconnect=true

[ipv4]
method=auto
may-fail=false

[ipv6]
method=disabled
EOF_NETWORK
chmod 0600 "$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection"

install -d -m 0755 \
  "$staging/etc/systemd/system/multi-user.target.wants" \
  "$staging/etc/systemd/system/network-online.target.wants" \
  "$staging/etc/systemd/system/getty.target.wants" \
  "$staging/etc/systemd/system/serial-getty@ttyS0.service.d"
ln -sfn /usr/lib/systemd/system/NetworkManager.service \
  "$staging/etc/systemd/system/multi-user.target.wants/NetworkManager.service"
ln -sfn /usr/lib/systemd/system/NetworkManager-wait-online.service \
  "$staging/etc/systemd/system/network-online.target.wants/NetworkManager-wait-online.service"
ln -sfn /usr/lib/systemd/system/serial-getty@.service \
  "$staging/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"
ln -sfn /usr/lib/systemd/system/sshd.service \
  "$staging/etc/systemd/system/multi-user.target.wants/sshd.service"

cat >"$staging/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF_GETTY'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF_GETTY

install -d -m 0755 "$staging/usr/local/sbin"
cat >"$staging/usr/local/sbin/firecrab-network-ready.sh" <<'EOF_SENTINEL'
#!/bin/sh
set -eu

ipv4=""
for _ in $(seq 1 15); do
    # Firecracker's default MMIO transport calls this interface eth0, while
    # Rocky's PCI transport assigns a predictable name such as ens2. The
    # profile above deliberately matches any Ethernet device, so readiness
    # must likewise use the first non-loopback global IPv4 address instead
    # of pinning a transport-specific device name.
    ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
        awk '$2 != "lo" { split($4, address, "/"); print address[1]; exit }')
    [ -n "$ipv4" ] && break
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
EOF_SENTINEL
chmod 0755 "$staging/usr/local/sbin/firecrab-network-ready.sh"

cat >"$staging/etc/systemd/system/firecrab-network-ready.service" <<'EOF_SERVICE'
[Unit]
Description=Firecrab network readiness sentinel
After=NetworkManager-wait-online.service
Wants=NetworkManager-wait-online.service

[Service]
Type=oneshot
StandardOutput=tty
TTYPath=/dev/console
ExecStart=/usr/local/sbin/firecrab-network-ready.sh

[Install]
WantedBy=multi-user.target
EOF_SERVICE
ln -sfn /etc/systemd/system/firecrab-network-ready.service \
  "$staging/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"

if [ -s /input/id_ed25519.pub ]; then
  install -d -m 0700 "$staging/root/.ssh"
  install -m 0600 /input/id_ed25519.pub "$staging/root/.ssh/authorized_keys"
fi

# EL9's kernel-install layout keeps the raw kernel under
# /usr/lib/modules/<version>/vmlinuz; unlike Debian-family packages it does
# not have to create a /boot/vmlinuz-* copy. Select that authoritative file
# and keep the initramfs in /boot where dracut writes it.
vmlinuz_path=$(find "$staging/usr/lib/modules" -mindepth 2 -maxdepth 2 -type f -name vmlinuz -printf '%p\n' | sort -V | tail -n 1)
[ -n "$vmlinuz_path" ] || fail 'Rocky kernel package did not install usr/lib/modules/*/vmlinuz'
kernel_version=$(basename "$(dirname "$vmlinuz_path")")
initrd_path="$staging/boot/initramfs-${kernel_version}.img"
kernel_config="$staging/usr/lib/modules/${kernel_version}/config"

# Guard each architecture's transport contract so a kernel package change
# cannot silently produce a template the runtime cannot boot.
virtio_drivers='virtio_blk virtio_net ext4'
case "$rocky_arch" in
  x86_64)
    grep -Eq '^CONFIG_VIRTIO_PCI=(y|m)$' "$kernel_config" \
      || fail "Rocky x86_64 kernel lacks CONFIG_VIRTIO_PCI: ${kernel_config}"
    if grep -q '^CONFIG_VIRTIO_PCI=m$' "$kernel_config"; then
      virtio_drivers="${virtio_drivers} virtio_pci"
    fi
    ;;
  aarch64)
    grep -Eq '^CONFIG_VIRTIO_MMIO=(y|m)$' "$kernel_config" \
      || fail "Rocky aarch64 kernel lacks CONFIG_VIRTIO_MMIO: ${kernel_config}"
    if grep -q '^CONFIG_VIRTIO_MMIO=m$' "$kernel_config"; then
      virtio_drivers="${virtio_drivers} virtio_mmio"
    fi
    ;;
esac

# A generic initramfs is necessary: it must not inherit the build host's
# host hardware and must contain the architecture-appropriate virtio
# transport plus block/network/ext4 modules needed before / is mounted. The
# temporary mounts are private to this privileged builder
# container; they let target-root dracut see normal /proc, /sys, /dev, and
# /run while retaining the guest's own kernel modules and dracut files.
info "building generic dracut initramfs for ${kernel_version}"
chroot "$staging" /usr/bin/dracut --force --no-hostonly \
  --add-drivers "$virtio_drivers" \
  "/boot/initramfs-${kernel_version}.img" "$kernel_version"
cleanup_chroot_mounts

[ -s "$initrd_path" ] || fail "dracut did not create ${initrd_path}"
test -e "$staging/etc/os-release" || fail 'missing /etc/os-release'
grep -Eq "^VERSION_ID=\"?${rocky_release//./\\.}\"?$" "$staging/etc/os-release" \
  || fail "Rocky rootfs is not pinned to VERSION_ID ${rocky_release}"
test -e "$staging/sbin/init" || fail 'missing /sbin/init'
test -x "$staging/usr/sbin/sshd" || fail 'missing sshd'
if [ -s /input/id_ed25519.pub ]; then
  test -s "$staging/root/.ssh/authorized_keys" || fail 'missing root authorized_keys'
fi
network_profile="$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection"
test -e "$network_profile" || fail 'missing transport-independent DHCP profile'
if grep -q '^interface-name=' "$network_profile"; then
  fail 'Rocky DHCP profile must not pin a Firecracker transport-specific interface name'
fi
test -e "$staging/etc/systemd/system/firecrab-network-ready.service" || fail 'missing network readiness service'

cp "$vmlinuz_path" /kernel-out/vmlinuz-rocky-raw
cp "$initrd_path" "/kernel-out/${initrd_image_name}"
chmod 0644 /kernel-out/vmlinuz-rocky-raw "/kernel-out/${initrd_image_name}"

# Dropped from the rootfs now that it is in /kernel-out: the runtime boots
# the initrd artifact this just produced, never one from inside the guest
# filesystem, so keeping the 27.9 MB duplicate only inflates every package.
# Same reasoning (and the same %ghost ownership) as
# bootstrap-rocky-in-guest.sh's copy of this step.
rm -f "$initrd_path"

rootfs_image="/out/${rootfs_image_name}"
rootfs_tmp="${rootfs_image}.tmp"
rm -f "$rootfs_tmp"
truncate -s "$rootfs_size" "$rootfs_tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$rootfs_tmp" >/dev/null
chmod 0644 "$rootfs_tmp"
mv "$rootfs_tmp" "$rootfs_image"

echo "ROOTFS_IMAGE=${rootfs_image}"
EOF
}

# True when the file is a raw Linux/arm64 Image: ARM64_IMAGE_MAGIC ("ARMd")
# at offset 0x38, the field Firecracker's loader checks before it jumps into
# the image. Read, never written — a wrapped kernel (a UKI or an EFI zboot
# image) with the magic stamped on top passes the loader and then dies
# silently in the guest instead of failing here.
is_arm64_image() {
  [ -s "$1" ] && [ "$(od -An -tx1 -j56 -N4 "$1" | tr -d ' \n')" = '41524d64' ]
}

prepare_kernel() {
  local raw_path="${kernel_artifact_dir}/vmlinuz-rocky-raw"
  local kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
  local kernel_image_tmp="${kernel_image_path}.tmp"

  [ -s "$raw_path" ] || fail "Rocky kernel was not copied out to ${raw_path}"
  if [ "$rocky_arch" = aarch64 ]; then
    # Rocky's arm64 vmlinuz is an EFI zboot image (a PE wrapper around a
    # gzip-compressed Image), which Firecracker cannot boot — unwrap it.
    info "extracting raw ARM64 Image from: ${raw_path}"
    if ! "$extract_arm64_image" "$raw_path" >"$kernel_image_tmp" \
      || ! is_arm64_image "$kernel_image_tmp"; then
      rm -f "$kernel_image_tmp"
      fail "extract-arm64-image could not extract a raw ARM64 Image from ${raw_path}"
    fi
  else
    info "extracting x86_64 ELF vmlinux from: ${raw_path}"
    if ! "$extract_vmlinux" "$raw_path" >"$kernel_image_tmp"; then
      rm -f "$kernel_image_tmp"
      fail "extract-vmlinux could not extract an ELF vmlinux from ${raw_path}"
    fi
    if ! file "$kernel_image_tmp" | grep -q 'ELF'; then
      rm -f "$kernel_image_tmp"
      fail "extracted kernel is not an ELF image: ${raw_path}"
    fi
  fi
  chmod 0644 "$kernel_image_tmp"
  mv "$kernel_image_tmp" "$kernel_image_path"
  rm -f "$raw_path"

  [ -s "${kernel_artifact_dir}/${initrd_image_name}" ] || \
    fail "Rocky initramfs was not copied out to ${kernel_artifact_dir}/${initrd_image_name}"
  info "Rocky kernel image: ${kernel_image_path}"
  info "Rocky initramfs: ${kernel_artifact_dir}/${initrd_image_name}"
}

cleanup_container_mounts() {
  local target

  for target in "${container_mounts[@]}"; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done
  container_mounts=()
}

mount_container_tree() {
  local source=$1
  local target=$2

  mkdir -p "$target"
  mount --rbind "$source" "$target"
  mount --make-rslave "$target"
  container_mounts=("$target" "${container_mounts[@]}")
}

download_container_base() {
  local archive_path=$1
  local checksum_path=$2
  local checksum_tmp="${checksum_path}.tmp"
  local expected_sha256=''

  if [ ! -s "$archive_path" ]; then
    info "downloading Rocky ${rocky_release} ${rocky_arch} Container-Base"
    curl -fsSL "$container_url" -o "${archive_path}.tmp" \
      || { rm -f "${archive_path}.tmp"; fail "Could not download ${container_url}"; }
    mv "${archive_path}.tmp" "$archive_path"
  else
    info "reusing Rocky Container-Base archive: ${archive_path}"
  fi

  if curl -fsSL "${container_url}.CHECKSUM" -o "$checksum_tmp"; then
    mv "$checksum_tmp" "$checksum_path"
  else
    rm -f "$checksum_tmp"
    [ -s "$checksum_path" ] \
      || fail "Could not download ${container_url}.CHECKSUM"
    info "reusing Rocky Container-Base checksum: ${checksum_path}"
  fi

  expected_sha256=$(sed -n "s/^SHA256 (${container_name}) = //p" "$checksum_path" | head -n 1)
  if [ -z "$expected_sha256" ]; then
    expected_sha256=$(awk -v file="$container_name" '$2 == file || $2 == "*" file { print $1; exit }' "$checksum_path")
  fi
  [ -n "$expected_sha256" ] \
    || fail "Could not parse the checksum for ${container_name}"
  printf '%s  %s\n' "$expected_sha256" "$archive_path" | sha256sum -c - >/dev/null \
    || fail 'Rocky Container-Base checksum verification failed'
}

extract_container_base() {
  local archive_path=$1
  local container_root=$2
  local archive_tree=$3
  local manifest_digest=''
  local -a layers=()
  local layer

  rm -rf "$container_root" "$archive_tree"
  mkdir -p "$container_root" "$archive_tree"
  tar -xJf "$archive_path" -C "$archive_tree"

  if [ -f "$archive_tree/index.json" ]; then
    manifest_digest=$(jq -r '.manifests[0].digest | sub("^sha256:"; "")' \
      "$archive_tree/index.json")
    if [ -z "$manifest_digest" ] || [ "$manifest_digest" = null ]; then
      fail 'Could not read Container-Base manifest digest from index.json'
    fi
    mapfile -t layers < <(
      jq -r '.layers[].digest | sub("^sha256:"; "")' \
        "$archive_tree/blobs/sha256/${manifest_digest}"
    )
    [ "${#layers[@]}" -gt 0 ] || fail 'Container-Base OCI manifest has no layers'
    for layer in "${layers[@]}"; do
      [ -f "$archive_tree/blobs/sha256/${layer}" ] \
        || fail "Container-Base layer is missing: ${layer}"
      tar -xf "$archive_tree/blobs/sha256/${layer}" -C "$container_root"
    done
  elif [ -f "$archive_tree/manifest.json" ]; then
    mapfile -t layers < <(jq -r '.[0].Layers[]' "$archive_tree/manifest.json")
    [ "${#layers[@]}" -gt 0 ] || fail 'Container-Base Docker manifest has no layers'
    for layer in "${layers[@]}"; do
      [ -f "$archive_tree/$layer" ] || fail "Container-Base layer is missing: ${layer}"
      tar -xf "$archive_tree/$layer" -C "$container_root"
    done
  else
    fail 'Container-Base is neither an OCI nor Docker archive'
  fi

  [ -x "$container_root/usr/bin/dnf" ] || fail 'Container-Base is missing usr/bin/dnf'
  [ -x "$container_root/bin/bash" ] || fail 'Container-Base is missing bin/bash'
}

restore_output_ownership() {
  if [ -z "${SUDO_UID:-}" ] || [ -z "${SUDO_GID:-}" ]; then
    return 0
  fi

  chown "${SUDO_UID}:${SUDO_GID}" \
    "${artifact_dir}/${rootfs_image_name}" \
    "${kernel_artifact_dir}/${kernel_image_name}" \
    "${kernel_artifact_dir}/${initrd_image_name}"
  chmod u+rw,go+r \
    "${artifact_dir}/${rootfs_image_name}" \
    "${kernel_artifact_dir}/${kernel_image_name}" \
    "${kernel_artifact_dir}/${initrd_image_name}"
}

main() {
  [ "$#" -eq 0 ] || fail 'install-rocky-rootfs.sh does not accept arguments.'

  for command in awk chmod chown chroot cp curl file find grep head id install jq \
    mkdir mount mv od rm sed sha256sum sort tail tar truncate umount uname xz; do
    require_command "$command"
  done
  if [ "$rocky_arch" = x86_64 ]; then
    [ -x "$extract_vmlinux" ] || fail "extract-vmlinux helper not found or not executable: ${extract_vmlinux}"
  else
    [ -x "$extract_arm64_image" ] || fail "extract-arm64-image helper not found or not executable: ${extract_arm64_image}"
  fi

  if [ "$(id -u)" -ne 0 ]; then
    require_command sudo
    exec sudo env \
      "M2IMAGE_ARCH=${M2IMAGE_ARCH:-}" \
      "M2IMAGE_DISTRO_SERIES=${M2IMAGE_DISTRO_SERIES:-9.8}" \
      "M2IMAGE_DISTRO_VERSION=${rocky_release}" \
      "M2IMAGE_ALIAS=${m2image_alias}" \
      "M2IMAGE_SBOM_OUTPUT=${sbom_output}" \
      "ROCKY_REPOSITORY_BASE=${rocky_repository_base}" \
      "ROCKY_CONTAINER_BUILD=${rocky_container_build}" \
      "FIRECRAB_SSH_PUBLIC_KEY=${FIRECRAB_SSH_PUBLIC_KEY:-}" \
      "$script_path"
  fi

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  kernel_artifact_dir=$(abs_dir "$kernel_artifact_dir")
  verify_native_architecture
  trap cleanup_container_mounts EXIT

  local ssh_public_key
  ssh_public_key=$(resolve_ssh_public_key)
  local staging_dir="${build_dir}/mnt"
  local configure_script="${build_dir}/configure.sh"
  local download_dir="${build_dir}/downloads"
  local archive_path="${download_dir}/${container_name}"
  local checksum_path="${download_dir}/${container_name}.CHECKSUM"
  local archive_tree="${build_dir}/container-archive"
  local container_root="${build_dir}/container-root"

  mkdir -p "$download_dir" "$staging_dir"
  download_container_base "$archive_path" "$checksum_path"
  info "extracting Rocky ${rocky_release} ${rocky_arch} Container-Base tarball"
  extract_container_base "$archive_path" "$container_root" "$archive_tree"

  write_configure_script "$configure_script"
  install -m 0755 "$configure_script" "$container_root/configure.sh"
  install -d -m 0755 "$container_root/input"
  install -m 0644 "$ssh_public_key" "$container_root/input/id_ed25519.pub"
  rm -f "$container_root/etc/resolv.conf"
  cp /etc/resolv.conf "$container_root/etc/resolv.conf"

  mkdir -p "$container_root/proc"
  mount -t proc proc "$container_root/proc"
  container_mounts=("$container_root/proc" "${container_mounts[@]}")
  mount_container_tree /sys "$container_root/sys"
  mount_container_tree /dev "$container_root/dev"
  mount_container_tree /run "$container_root/run"
  mount_container_tree "$staging_dir" "$container_root/work/rootfs"
  mount_container_tree "$artifact_dir" "$container_root/out"
  mount_container_tree "$kernel_artifact_dir" "$container_root/kernel-out"

  info "building Rocky Linux ${rocky_release} ${rocky_arch} via native chroot + direct ext4"
  chroot "$container_root" /bin/bash /configure.sh \
      "$rootfs_size" "$rootfs_hostname" "$rootfs_packages" \
      "$initrd_image_name" "$rocky_release" "$rocky_arch" "$rootfs_image_name" \
      "$rocky_repository_base"

  if [ -n "$sbom_output" ]; then
    [ -n "$m2image_alias" ] || fail 'M2IMAGE_ALIAS is required with M2IMAGE_SBOM_OUTPUT'
    require_command python3
    local package_db="${build_dir}/rpm-packages.tsv"
    chroot "$container_root" /usr/bin/rpm --root /work/rootfs -qa \
      --qf '%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\t%{LICENSE}\t%{SOURCERPM}\n' \
      >"$package_db"
    [ -s "$package_db" ] || fail 'Rocky rpm package query returned no installed packages'
    python3 "$sbom_generator" \
      --format rpm-tsv --distribution rocky \
      --image-alias "$m2image_alias" --image-version "$rocky_release" \
      --architecture "$rocky_arch" \
      --package-db "$package_db" --output "$sbom_output"
    if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
      chown "${SUDO_UID}:${SUDO_GID}" "$sbom_output"
    fi
  fi

  prepare_kernel
  [ -s "${artifact_dir}/${rootfs_image_name}" ] || \
    fail "Rocky rootfs image was not created: ${artifact_dir}/${rootfs_image_name}"
  restore_output_ownership
  info "Rocky rootfs image: ${artifact_dir}/${rootfs_image_name}"
}

main "$@"
