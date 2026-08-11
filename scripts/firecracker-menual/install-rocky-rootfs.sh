#!/usr/bin/env bash
# Build a Firecracker-ready version-pinned Rocky Linux rootfs with its EL9 kernel.
#
# The result deliberately stays a direct ext4 file rather than Rocky's cloud
# QCOW2 image: firecrab resizes and customizes rootfs files with e2fsprogs.
# Everything privileged happens in a throwaway official Rocky container, so
# the host needs Docker but no root chroot or loop mounts.
#
# EL9 x86_64 uses virtio-pci because its kernel leaves CONFIG_VIRTIO_MMIO off.
# Rocky's aarch64 kernel supports Firecracker's normal virtio-mmio transport.
# Both initramfs variants carry their architecture's transport plus
# virtio_blk/net and ext4 for the guest rootfs.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)

artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
build_dir="${repo_dir}/build/rocky-rootfs"
rocky_release=${M2IMAGE_DISTRO_VERSION:-9.8}
rocky_repository_base=${ROCKY_REPOSITORY_BASE:-https://download.rockylinux.org/pub/rocky}
rootfs_size='2G'
rootfs_hostname='firecrab'
docker_bin='docker'
# Docker Hub publishes the official major-version tag as a multi-arch image;
# the guest itself is pinned independently to the manifest-selected
# BaseOS/AppStream URLs and rejected unless /etc/os-release matches.
docker_image='rockylinux:9'
extract_vmlinux="${script_dir}/extract-vmlinux"

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

resolve_ssh_public_key() {
  local candidate=''
  local sudo_home=''

  if [ -n "${SUDO_UID:-}" ] && command -v getent >/dev/null 2>&1; then
    sudo_home=$(getent passwd "$SUDO_UID" | cut -d: -f6 || true)
    if [ -n "$sudo_home" ]; then
      candidate="${sudo_home}/.ssh/id_ed25519.pub"
    fi
  fi
  if [ -z "$candidate" ]; then
    candidate="${HOME:-}/.ssh/id_ed25519.pub"
  fi

  [ -n "$candidate" ] && [ -f "$candidate" ] || \
    fail 'Host SSH public key not found: ~/.ssh/id_ed25519.pub'
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

# A generic initramfs is necessary: it must not inherit the Docker builder's
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
test -s "$staging/root/.ssh/authorized_keys" || fail 'missing root authorized_keys'
network_profile="$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection"
test -e "$network_profile" || fail 'missing transport-independent DHCP profile'
if grep -q '^interface-name=' "$network_profile"; then
  fail 'Rocky DHCP profile must not pin a Firecracker transport-specific interface name'
fi
test -e "$staging/etc/systemd/system/firecrab-network-ready.service" || fail 'missing network readiness service'

cp "$vmlinuz_path" /kernel-out/vmlinuz-rocky-raw
cp "$initrd_path" "/kernel-out/${initrd_image_name}"
chmod 0644 /kernel-out/vmlinuz-rocky-raw "/kernel-out/${initrd_image_name}"

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

prepare_kernel() {
  local raw_path="${kernel_artifact_dir}/vmlinuz-rocky-raw"
  local kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
  local kernel_image_tmp="${kernel_image_path}.tmp"

  [ -s "$raw_path" ] || fail "Rocky kernel was not copied out to ${raw_path}"
  if [ "$rocky_arch" = aarch64 ]; then
    info "preserving ARM64 PE kernel Image from: ${raw_path}"
    cp "$raw_path" "$kernel_image_tmp"
    if ! file "$kernel_image_tmp" | grep -Eq 'PE32\+.*(ARM64|Aarch64)'; then
      rm -f "$kernel_image_tmp"
      fail "Rocky aarch64 kernel is not a PE32+ ARM64 Image: ${raw_path}"
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

main() {
  [ "$#" -eq 0 ] || fail 'install-rocky-rootfs.sh does not accept arguments.'

  for command in cp docker file find grep mkdir mv rm sort tail uname; do
    require_command "$command"
  done
  if [ "$rocky_arch" = x86_64 ]; then
    [ -x "$extract_vmlinux" ] || fail "extract-vmlinux helper not found or not executable: ${extract_vmlinux}"
  fi

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  kernel_artifact_dir=$(abs_dir "$kernel_artifact_dir")
  local ssh_public_key
  ssh_public_key=$(resolve_ssh_public_key)
  local staging_dir="${build_dir}/mnt"
  local configure_script="${build_dir}/configure.sh"
  mkdir -p "$staging_dir"
  write_configure_script "$configure_script"

  info "building Rocky Linux ${rocky_release} ${rocky_arch} rootfs via official ${docker_image} + BaseOS/AppStream"
  # dracut runs inside the newly-installed guest root and needs private bind
  # mounts for /proc, /sys, /dev, and /run. Mount needs SYS_ADMIN plus the
  # Docker profiles relaxed for that syscall; this remains narrower than a
  # privileged container, and all mounts stay in the throwaway container's
  # mount namespace.
  "$docker_bin" run --rm \
    --cap-add=SYS_ADMIN \
    --security-opt apparmor=unconfined \
    --security-opt seccomp=unconfined \
    -v "${configure_script}:/configure.sh:ro" \
    -v "${ssh_public_key}:/input/id_ed25519.pub:ro" \
    -v "${staging_dir}:/work/rootfs" \
    -v "${artifact_dir}:/out" \
    -v "${kernel_artifact_dir}:/kernel-out" \
    "$docker_image" bash /configure.sh "$rootfs_size" "$rootfs_hostname" "$rootfs_packages" \
      "$initrd_image_name" "$rocky_release" "$rocky_arch" "$rootfs_image_name" \
      "$rocky_repository_base"

  prepare_kernel
  [ -s "${artifact_dir}/${rootfs_image_name}" ] || \
    fail "Rocky rootfs image was not created: ${artifact_dir}/${rootfs_image_name}"
  info "Rocky rootfs image: ${artifact_dir}/${rootfs_image_name}"
}

main "$@"
