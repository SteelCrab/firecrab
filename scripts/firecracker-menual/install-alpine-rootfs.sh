#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/../.." && pwd -P)

alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
artifact_dir="${repo_dir}/images/rootfs"
build_dir="${repo_dir}/build/alpine-rootfs"
rootfs_size='512M'
rootfs_hostname='firecrab'

# `apk --root` installs straight into a staging directory without a mount or
# chroot, so building the image needs no host root — only a container able to
# write root-owned files/devnodes into that staging dir and into the
# root-owned images/rootfs/ (see install-ubuntu-roofs.sh's directory, created
# by that script's sudo re-exec). Docker gives us both without sudo.
docker_bin='docker'
docker_image='alpine:latest'
rootfs_packages='alpine-baselayout busybox openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps'

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
  case "$(uname -m 2>/dev/null || printf 'unknown')" in
    x86_64 | aarch64)
      uname -m
      ;;
    *)
      fail 'Unsupported architecture. Alpine rootfs creation supports x86_64 and aarch64.'
      ;;
  esac
}

resolve_ssh_public_key() {
  key_source="${HOME:-}/.ssh/id_ed25519.pub"

  if [ -z "${HOME:-}" ] || [ ! -f "$key_source" ]; then
    fail 'Host SSH public key not found: ~/.ssh/id_ed25519.pub'
  fi

  printf '%s\n' "$key_source"
}

# Alpine's per-arch release feed lists every flavor (minirootfs, netboot,
# uboot, ...); pick out the minirootfs record's branch/version/file/sha256.
resolve_alpine_minirootfs() {
  releases_url="${alpine_releases_base}/latest-stable/releases/${alpine_arch}/latest-releases.yaml"
  releases_yaml="${build_dir}/latest-releases.yaml"

  if ! curl -fsSL "$releases_url" -o "${releases_yaml}.tmp"; then
    fail "Could not download Alpine release metadata: ${releases_url}"
  fi
  mv "${releases_yaml}.tmp" "$releases_yaml"

  awk '
    function emit() { if (flavor == "alpine-minirootfs") { printf "%s %s %s %s\n", branch, version, file, sha256; found = 1 } }
    /^-[[:space:]]*$/ {
      emit()
      if (found) exit
      branch = ""; version = ""; file = ""; sha256 = ""; flavor = ""
      next
    }
    /^  branch:/ { branch = $2 }
    /^  version:/ { version = $2 }
    /^  flavor:/ { flavor = $2 }
    /^  file:/ { file = $2 }
    /^  sha256:/ { sha256 = $2 }
    END { if (!found) emit() }
  ' "$releases_yaml"
}

write_configure_script() {
  # Runs as root inside a throwaway Alpine container: extracts the verified
  # minirootfs archive, installs extra packages with `apk --root` (no mount
  # or chroot needed), configures a Firecracker serial-console boot, and
  # packages the result into an ext4 image written straight into /out.
  cat >"$1" <<'EOF'
#!/bin/sh
set -eu

staging=/work/rootfs
alpine_branch=$1
alpine_version=$2
alpine_arch=$3
hostname=$4
rootfs_size=$5
rootfs_packages=$6

mkdir -p "$staging"
tar -xzf /input/archive.tar.gz -C "$staging"

cat >"${staging}/etc/apk/repositories" <<REPOS
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/main
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/community
REPOS

# shellcheck disable=SC2086
apk add --no-cache --root "$staging" --update-cache $rootfs_packages

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
# (task-guest-network-configuration.md; guest agent/vsock is out of this
# project's competition scope).
cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF_SENTINEL'
#!/sbin/openrc-run

description="Firecrab network readiness sentinel"

depend() {
    need net
    after dhcpcd
}

start() {
    ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
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

if [ -s /input/id_ed25519.pub ]; then
  install -d -m 0700 "${staging}/root/.ssh"
  install -m 0600 /input/id_ed25519.pub "${staging}/root/.ssh/authorized_keys"
fi

test -e "${staging}/etc/os-release" || { echo 'missing /etc/os-release' >&2; exit 1; }
test -e "${staging}/bin/sh" || { echo 'missing /bin/sh' >&2; exit 1; }
test -e "${staging}/sbin/init" || { echo 'missing /sbin/init' >&2; exit 1; }
{ test -e "${staging}/sbin/agetty" || test -e "${staging}/usr/sbin/agetty"; } || { echo 'missing agetty' >&2; exit 1; }
test -e "${staging}/sbin/openrc" || { echo 'missing openrc' >&2; exit 1; }
test -e "${staging}/usr/sbin/sshd" || { echo 'missing sshd' >&2; exit 1; }
test -x "${staging}/etc/init.d/firecrab-network-ready" || { echo 'missing firecrab-network-ready init script' >&2; exit 1; }
test -L "${staging}/etc/runlevels/default/firecrab-network-ready" || { echo 'firecrab-network-ready not enabled in default runlevel' >&2; exit 1; }

apk add --no-cache e2fsprogs >/dev/null

rootfs_image="/out/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4"
tmp_image="${rootfs_image}.tmp"
truncate -s "$rootfs_size" "$tmp_image"
mkfs.ext4 -F -L rootfs -d "$staging" "$tmp_image" >/dev/null
mv "$tmp_image" "$rootfs_image"
ln -sfn "$(basename "$rootfs_image")" /out/alpine-rootfs.ext4
chown 1000:1000 "$rootfs_image" 2>/dev/null || true

echo "ROOTFS_IMAGE=${rootfs_image}"
EOF
}

main() {
  if [ "$#" -ne 0 ]; then
    fail 'install-alpine-rootfs.sh does not accept arguments.'
  fi

  require_command awk
  require_command curl
  require_command grep
  require_command mkdir
  require_command mv
  require_command sha256sum
  require_command uname
  require_command "$docker_bin"

  build_dir=$(abs_dir "$build_dir")
  artifact_dir=$(abs_dir "$artifact_dir")
  alpine_arch=$(detect_alpine_arch)
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

  # A prior run's staging tree is root-owned (apk --root writes real root:root
  # ownership so the boot image is faithful), so clearing it needs the same
  # container-root trick as creating it.
  mount_dir="${build_dir}/mnt"
  mkdir -p "$mount_dir"
  "$docker_bin" run --rm -v "${mount_dir}:/work/rootfs" "$docker_image" sh -c 'rm -rf /work/rootfs/* /work/rootfs/.[!.]* 2>/dev/null || true'

  info 'building Alpine rootfs staging + ext4 image via Docker (apk --root, no host root required)'
  "$docker_bin" run --rm \
    -v "${archive_path}:/input/archive.tar.gz:ro" \
    -v "${ssh_public_key}:/input/id_ed25519.pub:ro" \
    -v "${configure_script}:/configure.sh:ro" \
    -v "${mount_dir}:/work/rootfs" \
    -v "${artifact_dir}:/out" \
    "$docker_image" sh /configure.sh "$alpine_branch" "$alpine_version" "$alpine_arch" "$rootfs_hostname" "$rootfs_size" "$rootfs_packages"

  rootfs_image="${artifact_dir}/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4"
  rootfs_link="${artifact_dir}/alpine-rootfs.ext4"

  if [ ! -f "$rootfs_image" ]; then
    fail "Alpine rootfs image was not created: ${rootfs_image}"
  fi

  info "Alpine rootfs image created: ${rootfs_image}"
  info "Alpine rootfs symlink: ${rootfs_link} -> $(basename "$rootfs_image")"
}

main "$@"
