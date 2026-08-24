#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)
script_path="${script_dir}/$(basename -- "$0")"

alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
alpine_series=${M2IMAGE_DISTRO_SERIES:-3.24}
alpine_version_setting=${M2IMAGE_DISTRO_VERSION:-3.24.1}
artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
kernel_image_name=''
initrd_image_name=''
extract_vmlinux="${script_dir}/extract-vmlinux"
extract_arm64_image="${script_dir}/extract-arm64-image"
build_dir="${repo_dir}/build/alpine-rootfs"
rootfs_size='512M'
rootfs_hostname='firecrab'
m2image_alias=${M2IMAGE_ALIAS:-}
sbom_output=${M2IMAGE_SBOM_OUTPUT:-}
sbom_generator="${repo_dir}/scripts/m2image_sbom.py"

# Like the Ubuntu builder, this script expands the official base archive into
# a staging directory, installs packages with the distribution's own package
# manager in a temporary chroot, then creates ext4 directly with mkfs.ext4 -d.
# No Docker daemon or OCI runtime is involved.
# linux-virt: Alpine's own officially-maintained cloud/virt kernel package
# (public-docs/images.md) — replaces the self-built vanilla kernel
# every template used to share. Unlike Ubuntu's linux-image-generic,
# virtio_blk/ext4 are modules here rather than builtin, so the initramfs-virt
# Alpine builds alongside it (mkinitfs) has to ship as the VM's initrd too —
# without it the kernel can never reach /dev/vda to mount the real root.
# bash: Shell repository scripts often use #!/bin/bash (same as Ubuntu).
# spdx-licenses-text: Alpine's normal runtime packages intentionally omit most
# license files. Keep the distribution-packaged SPDX License List text corpus
# in the image so release packaging can recover a complete canonical license
# bundle without fetching mutable web content or inventing license text.
rootfs_packages='alpine-baselayout busybox bash openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt spdx-licenses-text'

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

detect_alpine_arch() {
  case "${M2IMAGE_ARCH:-$(uname -m 2>/dev/null || printf 'unknown')}" in
    x86_64 | amd64)
      printf '%s\n' 'x86_64'
      ;;
    aarch64 | arm64)
      printf '%s\n' 'aarch64'
      ;;
    *)
      fail 'Unsupported architecture. Alpine rootfs creation supports x86_64 and aarch64.'
      ;;
  esac
}

verify_native_architecture() {
  local host_arch=''

  case "$(uname -m 2>/dev/null || printf unknown)" in
    x86_64 | amd64) host_arch='x86_64' ;;
    aarch64 | arm64) host_arch='aarch64' ;;
    *) fail 'Unsupported build host architecture.' ;;
  esac
  [ "$host_arch" = "$alpine_arch" ] \
    || fail "Host-native Alpine chroot requires a ${alpine_arch} host (current: ${host_arch})."
}

resolve_ssh_public_key() {
  local key_source=${FIRECRAB_SSH_PUBLIC_KEY:-}
  local key_home=''

  if [ -n "$key_source" ]; then
    [ -s "$key_source" ] || fail "FIRECRAB_SSH_PUBLIC_KEY is not a readable public key: ${key_source}"
    printf '%s\n' "$key_source"
    return
  fi

  if [ -n "${SUDO_UID:-}" ] && has_command getent; then
    key_home=$(getent passwd "$SUDO_UID" | cut -d: -f6 || true)
  elif [ -n "${HOME:-}" ]; then
    key_home=$HOME
  fi

  if [ -n "$key_home" ]; then
    for key_source in \
      "$key_home/.ssh/id_ed25519.pub" \
      "$key_home/.ssh/id_ecdsa.pub" \
      "$key_home/.ssh/id_rsa.pub"; do
      if [ -s "$key_source" ]; then
        printf '%s\n' "$key_source"
        return
      fi
    done
  fi

  # SSH is optional: every Firecrab guest has an autologin serial console.
  # Keep an empty source so the configure path stays deterministic without
  # modifying the operator's ~/.ssh directory behind their back.
  key_source="${build_dir}/no-authorized-key.pub"
  : >"$key_source"
  info 'no host SSH public key found; building with serial-console access only' >&2
  printf '%s\n' "$key_source"
}

# Resolve the exact manifest-pinned minirootfs. A branch's
# latest-releases.yaml changes whenever Alpine publishes a patch release, so
# using it would make an alias such as alpine-3.24.1 silently change contents
# while package paths and runtime specs still expect 3.24.1.
resolve_alpine_minirootfs() {
  local branch="v${alpine_series}"
  local archive_name="alpine-minirootfs-${alpine_version_setting}-${alpine_arch}.tar.gz"
  local checksum_url="${alpine_releases_base}/${branch}/releases/${alpine_arch}/${archive_name}.sha256"
  local checksum_file="${build_dir}/${archive_name}.sha256"
  local checksum=''

  if ! curl -fsSL "$checksum_url" -o "${checksum_file}.tmp"; then
    fail "Could not download Alpine checksum: ${checksum_url}"
  fi
  mv "${checksum_file}.tmp" "$checksum_file"
  checksum=$(awk -v file="$archive_name" '$2 == file || $2 == "*" file { print $1; exit }' "$checksum_file")
  [ -n "$checksum" ] || fail "Could not find ${archive_name} in ${checksum_url}"
  printf '%s %s %s %s\n' "$branch" "$alpine_version_setting" "$archive_name" "$checksum"
}

write_configure_script() {
  # Runs as root on the host: extracts the verified minirootfs archive, uses
  # its own apk inside a temporary chroot, configures the guest, and writes a
  # direct ext4 image. All paths are explicit and remain inside the repository.
  cat >"$1" <<'EOF'
#!/bin/sh
set -eu

staging=$1
archive_path=$2
ssh_public_key=$3
kernel_out=$4
out=$5
alpine_branch=$6
alpine_version=$7
alpine_arch=$8
hostname=$9
rootfs_size=${10}
rootfs_packages=${11}
initrd_image_name=${12}

chroot_mounts=''

cleanup_chroot_mounts() {
  target=''
  for target in $chroot_mounts; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done
  chroot_mounts=''
}

mount_chroot_fs() {
  mount -t proc proc "$staging/proc"
  chroot_mounts="$staging/proc $chroot_mounts"
  mount --rbind /sys "$staging/sys"
  mount --make-rslave "$staging/sys"
  chroot_mounts="$staging/sys $chroot_mounts"
  mount --rbind /dev "$staging/dev"
  mount --make-rslave "$staging/dev"
  chroot_mounts="$staging/dev $chroot_mounts"
}

trap cleanup_chroot_mounts EXIT

rm -rf "$staging"
mkdir -p "$staging"
tar --numeric-owner -xzf "$archive_path" -C "$staging"

cat >"${staging}/etc/apk/repositories" <<REPOS
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/main
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/community
REPOS

# Use the downloaded distribution's own apk rather than requiring apk on the
# host. The temporary resolver is replaced with the guest resolver below.
cp /etc/resolv.conf "${staging}/etc/resolv.conf"
mount_chroot_fs
# shellcheck disable=SC2086
chroot "$staging" /sbin/apk add --no-cache --update-cache $rootfs_packages
cleanup_chroot_mounts

cat >"${staging}/etc/hostname" <<EOF_HOSTNAME
${hostname}
EOF_HOSTNAME

cat >"${staging}/etc/hosts" <<EOF_HOSTS
127.0.0.1 localhost
127.0.1.1 ${hostname}
EOF_HOSTS

cat >"${staging}/etc/fstab" <<'EOF_FSTAB'
/dev/vda / ext4 defaults 0 1
EOF_FSTAB

# firecrab-net-helper's dnsmasq answers DNS on the bridge gateway itself
# (172.30.0.1) for every guest on the VPC subnet — dhcpcd overwrites this
# from the DHCP-provided options once it runs, so this is really just the
# pre-DHCP fallback value.
cat >"${staging}/etc/resolv.conf" <<'EOF_RESOLV'
nameserver 172.30.0.1
EOF_RESOLV

install -d -m 0755 "${staging}/etc/network"
cat >"${staging}/etc/network/interfaces" <<'EOF_IFACES'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
EOF_IFACES

# Prints a fixed sentinel line to /dev/console (Firecracker's captured
# stdout) once DHCP + DNS are confirmed working — the signal firecrab-api's
# start pipeline waits on in place of a guest agent event
# (public-docs/networking.md; guest agent/vsock is out of this
# project's competition scope).
cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF_SENTINEL'
#!/sbin/openrc-run

description="Firecrab network readiness sentinel"

depend() {
    need net
    after dhcpcd
}

start() {
    # `after dhcpcd` only orders service *starts* — dhcpcd's own start
    # action returns as soon as it forks into the background, well before
    # it has actually completed a DHCP transaction, so checking eth0 right
    # away here routinely runs before the lease exists yet. Poll briefly
    # instead of trusting the ordering dependency to mean "has an address".
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
}
EOF_SENTINEL
chmod 0755 "${staging}/etc/init.d/firecrab-network-ready"

# Serial console getty with autologin, mirroring the Ubuntu agetty setup.
grep -v '^ttyS0::' "${staging}/etc/inittab" >"${staging}/etc/inittab.new"
printf 'ttyS0::respawn:/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 vt100\n' \
  >>"${staging}/etc/inittab.new"
mv "${staging}/etc/inittab.new" "${staging}/etc/inittab"

# Standard OpenRC runlevels for a minimal single-disk VM. hwclock is
# deliberately left out: Firecracker exposes no RTC device, so it only
# fails and drags in modprobe noise for a /lib/modules that doesn't exist
# (this kernel has no loadable module support).
mkdir -p "${staging}/etc/runlevels/sysinit" "${staging}/etc/runlevels/boot" "${staging}/etc/runlevels/default"
for svc in devfs dmesg; do
  ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/sysinit/${svc}"
done
for svc in hostname bootmisc sysctl loopback; do
  ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/boot/${svc}"
done
for svc in local dhcpcd sshd firecrab-network-ready; do
  ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/default/${svc}"
done

if [ -s "$ssh_public_key" ]; then
  install -d -m 0700 "${staging}/root/.ssh"
  install -m 0600 "$ssh_public_key" "${staging}/root/.ssh/authorized_keys"
fi

test -e "${staging}/etc/os-release" || { echo 'missing /etc/os-release' >&2; exit 1; }
test -e "${staging}/bin/sh" || { echo 'missing /bin/sh' >&2; exit 1; }
test -e "${staging}/sbin/init" || { echo 'missing /sbin/init' >&2; exit 1; }
{ test -e "${staging}/sbin/agetty" || test -e "${staging}/usr/sbin/agetty"; } || { echo 'missing agetty' >&2; exit 1; }
test -e "${staging}/sbin/openrc" || { echo 'missing openrc' >&2; exit 1; }
test -e "${staging}/usr/sbin/sshd" || { echo 'missing sshd' >&2; exit 1; }
test -x "${staging}/etc/init.d/firecrab-network-ready" || { echo 'missing firecrab-network-ready init script' >&2; exit 1; }
test -L "${staging}/etc/runlevels/default/firecrab-network-ready" || { echo 'firecrab-network-ready not enabled in default runlevel' >&2; exit 1; }

# linux-virt's boot files land under the staging root, not this container's
# own /boot — pulled out to /kernel-out (mounted from the host) so the host
# side can prepare the architecture-specific Firecracker kernel. x86_64
# needs an uncompressed ELF vmlinux; ARM64 needs the raw Image unwrapped out
# of the EFI zboot wrapper Alpine ships.
test -e "${staging}/boot/vmlinuz-virt" || { echo 'missing boot/vmlinuz-virt (linux-virt)' >&2; exit 1; }
test -e "${staging}/boot/initramfs-virt" || { echo 'missing boot/initramfs-virt (linux-virt)' >&2; exit 1; }
cp "${staging}/boot/vmlinuz-virt" "${kernel_out}/vmlinuz-virt-raw"
cp "${staging}/boot/initramfs-virt" "${kernel_out}/${initrd_image_name}"

rootfs_image="${out}/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4"
tmp_image="${rootfs_image}.tmp"
truncate -s "$rootfs_size" "$tmp_image"
if grep -qw orphan_file /etc/mke2fs.conf 2>/dev/null; then
  mkfs.ext4 -F -O '^orphan_file' -L rootfs -d "$staging" "$tmp_image" >/dev/null
else
  mkfs.ext4 -F -L rootfs -d "$staging" "$tmp_image" >/dev/null
fi
mv "$tmp_image" "$rootfs_image"
ln -sfn "$(basename "$rootfs_image")" "${out}/alpine-rootfs.ext4"

echo "ROOTFS_IMAGE=${rootfs_image}"
EOF
}

restore_output_ownership() {
  if [ -z "${SUDO_UID:-}" ] || [ -z "${SUDO_GID:-}" ]; then
    return
  fi

  chown "${SUDO_UID}:${SUDO_GID}" \
    "${artifact_dir}/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4" \
    "${kernel_artifact_dir}/${kernel_image_name}" \
    "${kernel_artifact_dir}/${initrd_image_name}"
  chown -h "${SUDO_UID}:${SUDO_GID}" "${artifact_dir}/alpine-rootfs.ext4" 2>/dev/null || true
}

# True when the file is a raw Linux/arm64 Image: ARM64_IMAGE_MAGIC ("ARMd")
# at offset 0x38, the field Firecracker's loader checks before it jumps into
# the image. Read, never written — a wrapped kernel (a UKI or an EFI zboot
# image) with the magic stamped on top passes the loader and then dies
# silently in the guest instead of failing here.
is_arm64_image() {
  [ -s "$1" ] && [ "$(od -An -tx1 -j56 -N4 "$1" | tr -d ' \n')" = '41524d64' ]
}

# Prepares the raw vmlinuz-virt copied out to /kernel-out. Firecracker expects
# an uncompressed ELF vmlinux on x86_64 and a raw Linux/arm64 Image on ARM64.
# Alpine's linux-virt arm64 vmlinuz is neither: it is an EFI zboot image, a PE
# wrapper around a gzip-compressed Image, so it has to be unwrapped.
extract_kernel() {
  raw_path="${kernel_artifact_dir}/vmlinuz-virt-raw"
  if [ ! -s "$raw_path" ]; then
    fail "linux-virt's vmlinuz-virt was not copied out to ${raw_path}"
  fi

  kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
  kernel_image_tmp="${kernel_image_path}.tmp"
  if [ "$alpine_arch" = aarch64 ]; then
    info "extracting raw ARM64 Image from: ${raw_path}"
    if ! "$extract_arm64_image" "$raw_path" >"$kernel_image_tmp" \
      || ! is_arm64_image "$kernel_image_tmp"; then
      rm -f "$kernel_image_tmp"
      fail "extract-arm64-image could not extract a raw ARM64 Image from ${raw_path}"
    fi
  else
    info "extracting ELF vmlinux from: ${raw_path}"
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
  info "Alpine kernel image: ${kernel_image_path}"

  initrd_image_path="${kernel_artifact_dir}/${initrd_image_name}"
  if [ ! -s "$initrd_image_path" ]; then
    fail "linux-virt's initramfs-virt was not copied out to ${initrd_image_path}"
  fi
  chmod 0644 "$initrd_image_path"
  info "Alpine initrd image: ${initrd_image_path}"
}

main() {
  if [ "$#" -ne 0 ]; then
    fail 'install-alpine-rootfs.sh does not accept arguments.'
  fi

  require_command awk
  require_command cp
  require_command curl
  require_command file
  require_command gzip
  require_command grep
  require_command chroot
  require_command id
  require_command install
  require_command mkdir
  require_command mkfs.ext4
  require_command mount
  require_command mv
  require_command od
  require_command rm
  require_command sha256sum
  require_command tar
  require_command truncate
  require_command umount
  require_command uname
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
      "M2IMAGE_DISTRO_SERIES=${alpine_series}" \
      "M2IMAGE_DISTRO_VERSION=${alpine_version_setting}" \
      "M2IMAGE_ALIAS=${m2image_alias}" \
      "M2IMAGE_SBOM_OUTPUT=${sbom_output}" \
      "FIRECRAB_SSH_PUBLIC_KEY=${FIRECRAB_SSH_PUBLIC_KEY:-}" \
      "$script_path"
  fi

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  alpine_arch=$(detect_alpine_arch)
  verify_native_architecture
  if [ "$alpine_arch" = aarch64 ]; then
    kernel_image_name='Image-alpine-virt-aarch64'
  else
    kernel_image_name='vmlinux-alpine-virt-x86_64'
  fi
  initrd_image_name="initramfs-alpine-virt-${alpine_arch}"
  ssh_public_key=$(resolve_ssh_public_key)

  info "Alpine architecture: ${alpine_arch}"
  read -r alpine_branch alpine_version archive_name archive_sha256 < <(resolve_alpine_minirootfs)
  if [ -z "$alpine_branch" ] || [ -z "$archive_name" ]; then
    fail "Could not resolve the Alpine minirootfs release for ${alpine_arch}."
  fi
  info "Alpine branch: ${alpine_branch}"
  info "Alpine minirootfs version: ${alpine_version}"

  download_dir="${build_dir}/downloads"
  archive_path="${download_dir}/${archive_name}"
  mkdir -p "$download_dir"

  if [ -f "$archive_path" ]; then
    info "reusing Alpine minirootfs archive: ${archive_path}"
  else
    archive_url="${alpine_releases_base}/${alpine_branch}/releases/${alpine_arch}/${archive_name}"
    info "downloading Alpine minirootfs archive: ${archive_url}"
    if ! curl -fsSL "$archive_url" -o "${archive_path}.tmp"; then
      rm -f "${archive_path}.tmp"
      fail "Could not download Alpine minirootfs archive: ${archive_url}"
    fi
    mv "${archive_path}.tmp" "$archive_path"
  fi

  info 'verifying Alpine minirootfs archive checksum'
  printf '%s  %s\n' "$archive_sha256" "$archive_path" | sha256sum -c -

  configure_script="${build_dir}/configure.sh"
  write_configure_script "$configure_script"

  mount_dir="${build_dir}/mnt"
  rm -rf "$mount_dir"
  mkdir -p "$mount_dir"
  mkdir -p "$kernel_artifact_dir"

  info 'building Alpine rootfs staging + ext4 image via host-native chroot'
  sh "$configure_script" \
    "$mount_dir" "$archive_path" "$ssh_public_key" "$kernel_artifact_dir" "$artifact_dir" \
    "$alpine_branch" "$alpine_version" "$alpine_arch" "$rootfs_hostname" "$rootfs_size" \
    "$rootfs_packages" "$initrd_image_name"

  if [ -n "$sbom_output" ]; then
    [ -n "$m2image_alias" ] || fail 'M2IMAGE_ALIAS is required with M2IMAGE_SBOM_OUTPUT'
    require_command python3
    [ -f "$mount_dir/lib/apk/db/installed" ] \
      || fail 'Alpine package database missing from built staging root'
    python3 "$sbom_generator" \
      --format alpine --distribution alpine \
      --image-alias "$m2image_alias" --image-version "$alpine_version" \
      --architecture "$alpine_arch" \
      --package-db "$mount_dir/lib/apk/db/installed" --output "$sbom_output"
    if [ -n "${SUDO_UID:-}" ] && [ -n "${SUDO_GID:-}" ]; then
      chown "${SUDO_UID}:${SUDO_GID}" "$sbom_output"
    fi
  fi

  extract_kernel
  restore_output_ownership

  rootfs_image="${artifact_dir}/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4"
  rootfs_link="${artifact_dir}/alpine-rootfs.ext4"

  if [ ! -f "$rootfs_image" ]; then
    fail "Alpine rootfs image was not created: ${rootfs_image}"
  fi

  info "Alpine rootfs image created: ${rootfs_image}"
  info "Alpine rootfs symlink: ${rootfs_link} -> $(basename "$rootfs_image")"
}

main "$@"
