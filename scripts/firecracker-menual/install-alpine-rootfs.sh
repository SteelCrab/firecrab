#!/usr/bin/env bash
# Build a Firecracker-ready Alpine Linux rootfs from official minirootfs tarball.

set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)

alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
artifact_dir="${repo_dir}/images/rootfs"
kernel_artifact_dir="${repo_dir}/images/kernel"
extract_vmlinux="${script_dir}/extract-vmlinux"
build_dir="${repo_dir}/build/alpine-rootfs"
rootfs_size='512M'
rootfs_hostname='firecrab'

info() { printf '[INFO] %s\n' "$1"; }
fail() { printf '[FAIL] %s\n' "$1" >&2; exit 1; }

has_command() { command -v "$1" >/dev/null 2>&1; }
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
    x86_64 | amd64) printf '%s\n' 'x86_64' ;;
    aarch64 | arm64) printf '%s\n' 'aarch64' ;;
    *) fail 'Unsupported architecture. Alpine rootfs creation supports x86_64 and aarch64.' ;;
  esac
}

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

configure_alpine_rootfs() {
  local staging=$1

  cat >"${staging}/etc/apk/repositories" <<REPOS
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/main
https://dl-cdn.alpinelinux.org/alpine/${alpine_branch}/community
REPOS

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

  mkdir -p "${staging}/etc/network"
  cat >"${staging}/etc/network/interfaces" <<'EOF_IFACES'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
EOF_IFACES

  cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF_SENTINEL'
#!/sbin/openrc-run

description="Firecrab network readiness sentinel"

depend() {
    need net
    after dhcpcd
}

start() {
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

  if [ -f "${staging}/etc/inittab" ]; then
    grep -v '^ttyS0::' "${staging}/etc/inittab" >"${staging}/etc/inittab.new" || true
    printf 'ttyS0::respawn:/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 vt100\n' \
      >>"${staging}/etc/inittab.new"
    mv "${staging}/etc/inittab.new" "${staging}/etc/inittab"
  fi

  mkdir -p "${staging}/etc/runlevels/sysinit" "${staging}/etc/runlevels/boot" "${staging}/etc/runlevels/default"
  for svc in devfs dmesg; do
    [ -f "${staging}/etc/init.d/${svc}" ] && ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/sysinit/${svc}"
  done
  for svc in hostname bootmisc sysctl loopback; do
    [ -f "${staging}/etc/init.d/${svc}" ] && ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/boot/${svc}"
  done
  for svc in local dhcpcd sshd firecrab-network-ready; do
    [ -f "${staging}/etc/init.d/${svc}" ] && ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/default/${svc}"
  done

  local ssh_public_key
  ssh_public_key=$(resolve_ssh_public_key)
  if [ -n "$ssh_public_key" ] && [ -f "$ssh_public_key" ]; then
    mkdir -p -m 0700 "${staging}/root/.ssh"
    cp "$ssh_public_key" "${staging}/root/.ssh/authorized_keys"
    chmod 0600 "${staging}/root/.ssh/authorized_keys"
  fi
}

extract_alpine_kernel() {
  local staging=$1
  mkdir -p "$kernel_artifact_dir"

  local vmlinuz_src=""
  for candidate in "${staging}/boot/vmlinuz-virt" "${staging}/boot/vmlinuz" "${staging}/boot/vmlinux"; do
    if [ -f "$candidate" ]; then
      vmlinuz_src="$candidate"
      break
    fi
  done

  if [ -n "$vmlinuz_src" ]; then
    local kernel_image_path="${kernel_artifact_dir}/${kernel_image_name}"
    if [ "$alpine_arch" = aarch64 ]; then
      info "preserving Alpine ARM64 PE kernel Image: ${vmlinuz_src}"
      cp "$vmlinuz_src" "$kernel_image_path"
    else
      info "extracting Alpine ELF vmlinux kernel from: ${vmlinuz_src}"
      if [ -x "$extract_vmlinux" ]; then
        "$extract_vmlinux" "$vmlinuz_src" >"$kernel_image_path" || cp "$vmlinuz_src" "$kernel_image_path"
      else
        cp "$vmlinuz_src" "$kernel_image_path"
      fi
    fi
    chmod 0644 "$kernel_image_path"
  fi

  local initrd_src="${staging}/boot/initramfs-virt"
  if [ -f "$initrd_src" ]; then
    local initrd_image_path="${kernel_artifact_dir}/${initrd_image_name}"
    cp "$initrd_src" "$initrd_image_path"
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
  alpine_arch=$(detect_alpine_arch)

  if [ "$alpine_arch" = aarch64 ]; then
    kernel_image_name='Image-alpine-virt-aarch64'
  else
    kernel_image_name='vmlinux-alpine-virt-x86_64'
  fi
  initrd_image_name="initramfs-alpine-virt-${alpine_arch}"

  info "Alpine architecture: ${alpine_arch}"
  read -r alpine_branch alpine_version archive_name archive_sha256 < <(resolve_alpine_minirootfs)
  if [ -z "$alpine_branch" ] || [ -z "$archive_name" ]; then
    fail "Could not resolve Alpine minirootfs release for ${alpine_arch}."
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

  mount_dir="${build_dir}/mnt"
  rm -rf "$mount_dir"
  mkdir -p "$mount_dir"

  info 'extracting Alpine minirootfs into staging root'
  tar --numeric-owner -xpf "$archive_path" -C "$mount_dir"

  configure_alpine_rootfs "$mount_dir"
  extract_alpine_kernel "$mount_dir"

  rootfs_image="${artifact_dir}/alpine-rootfs-${alpine_version}-${alpine_arch}.ext4"
  rootfs_link="${artifact_dir}/alpine-rootfs.ext4"

  info "creating Alpine rootfs image: ${rootfs_image}"
  mkfs.ext4 -F -L rootfs -d "$mount_dir" "$rootfs_image" >/dev/null

  ln -sfn "$(basename "$rootfs_image")" "$rootfs_link"
  info "Alpine rootfs image created: ${rootfs_image}"
}

main "$@"
