#!/bin/sh
# Runs entirely inside a firecrab builder VM (any installed template) —
# downloads the official Alpine minirootfs, chroots in and installs
# packages/kernel via ITS OWN bundled apk (not the outer guest's), then
# packs the result into an ext4 image via `mkfs.ext4 -d` (no loop mount).
# Adapted from install-alpine-rootfs.sh's write_configure_script — same
# package list/config files, minus the outer-docker-container wrapper.
set -eu

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
rootfs_size='512M'
rootfs_hostname='firecrab'
rootfs_packages='alpine-baselayout busybox openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup_mounts() {
  umount -R "$staging/proc" 2>/dev/null || true
  umount -R "$staging/sys" 2>/dev/null || true
  umount -R "$staging/dev" 2>/dev/null || true
}
trap cleanup_mounts EXIT

rm -rf "$work"
mkdir -p "$staging" "$out"

arch=$(uname -m)
case "$arch" in
  x86_64) ;;
  *) fail "unsupported architecture: $arch" ;;
esac

info 'resolving latest Alpine 3.24 minirootfs release'
releases_yaml="$work/latest-releases.yaml"
curl -fsSL "${alpine_releases_base}/v3.24/releases/${arch}/latest-releases.yaml" -o "$releases_yaml" \
  || fail 'could not download Alpine release metadata'

# intentional word splitting: awk emits four space-separated fields to
# become positional parameters.
# shellcheck disable=SC2046
set -- $(awk '
  function emit() { if (flavor == "alpine-minirootfs") { printf "%s %s %s %s", branch, version, file, sha256; found = 1 } }
  /^-[[:space:]]*$/ { emit(); if (found) exit; branch=""; version=""; file=""; sha256=""; flavor=""; next }
  /^  branch:/ { branch = $2 }
  /^  version:/ { version = $2 }
  /^  flavor:/ { flavor = $2 }
  /^  file:/ { file = $2 }
  /^  sha256:/ { sha256 = $2 }
  END { if (!found) emit() }
' "$releases_yaml")
branch=$1
version=$2
archive_file=$3
archive_sha256=$4
[ -n "$branch" ] && [ -n "$archive_file" ] || fail 'could not resolve the Alpine minirootfs release'
info "Alpine branch ${branch}, minirootfs version ${version}"

archive_path="$work/${archive_file}"
curl -fsSL "${alpine_releases_base}/${branch}/releases/${arch}/${archive_file}" -o "$archive_path" \
  || fail 'could not download the Alpine minirootfs archive'
printf '%s  %s\n' "$archive_sha256" "$archive_path" | sha256sum -c - || fail 'minirootfs checksum mismatch'

info 'extracting minirootfs'
tar -xzf "$archive_path" -C "$staging"

cat >"${staging}/etc/apk/repositories" <<REPOS
${alpine_releases_base}/${branch}/main
${alpine_releases_base}/${branch}/community
REPOS

mount -t proc proc "$staging/proc"
mount --rbind /sys "$staging/sys"
mount --rbind /dev "$staging/dev"

info "installing packages: ${rootfs_packages}"
# package list is a deliberate whitespace list.
# shellcheck disable=SC2086
chroot "$staging" /sbin/apk add --no-cache --update-cache $rootfs_packages
chroot "$staging" /sbin/apk add --no-cache e2fsprogs

cat >"${staging}/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"${staging}/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"${staging}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
cat >"${staging}/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF
mkdir -p "${staging}/etc/network"
cat >"${staging}/etc/network/interfaces" <<'EOF'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
EOF

cat >"${staging}/etc/init.d/firecrab-network-ready" <<'EOF'
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
EOF
chmod 0755 "${staging}/etc/init.d/firecrab-network-ready"

grep -v '^ttyS0::' "${staging}/etc/inittab" >"${staging}/etc/inittab.new"
printf 'ttyS0::respawn:/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 vt100\n' \
  >>"${staging}/etc/inittab.new"
mv "${staging}/etc/inittab.new" "${staging}/etc/inittab"

mkdir -p "${staging}/etc/runlevels/sysinit" "${staging}/etc/runlevels/boot" "${staging}/etc/runlevels/default"
for svc in devfs dmesg; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/sysinit/${svc}"; done
for svc in hostname bootmisc sysctl loopback; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/boot/${svc}"; done
for svc in local dhcpcd sshd firecrab-network-ready; do ln -sf "/etc/init.d/${svc}" "${staging}/etc/runlevels/default/${svc}"; done

test -e "${staging}/boot/vmlinuz-virt" || fail 'missing boot/vmlinuz-virt (linux-virt)'
test -e "${staging}/boot/initramfs-virt" || fail 'missing boot/initramfs-virt (linux-virt)'
cp "${staging}/boot/vmlinuz-virt" "$out/vmlinuz-virt-raw"
cp "${staging}/boot/initramfs-virt" "$out/initramfs"

cleanup_mounts

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# extract-vmlinux ships alongside this script in the repo but does not
# exist inside the guest — the raw vmlinuz is dumped out as-is
# ($out/vmlinuz-virt-raw) and Task 8 runs extract-vmlinux on the HOST
# after pulling it out of the guest disk, since the host is already known
# to have every decompressor it might need (same reasoning
# install-alpine-rootfs.sh's own extract_kernel already used).
info 'bootstrap complete'
