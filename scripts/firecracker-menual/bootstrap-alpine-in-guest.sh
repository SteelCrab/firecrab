#!/bin/sh
# Runs entirely inside a firecrab MicroBoot builder VM — Alpine's own
# recovery shell (see crate::microboot's doc comment), NOT an arbitrary
# installed template: this script hard-depends on that environment via
# `apk add --initdb`, `udhcpc`, and busybox's umount quirks below.
# Downloads the official Alpine minirootfs, chroots in and installs
# packages/kernel via ITS OWN bundled apk (not the outer guest's), then
# packs the result into an ext4 image via `mkfs.ext4 -d` (no loop mount).
# Shares the host-native install-alpine-rootfs.sh package/configuration model,
# adapted to publish through the temporary MicroBoot builder disk.
set -eu

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
alpine_releases_base='https://dl-cdn.alpinelinux.org/alpine'
alpine_series='@M2IMAGE_DISTRO_SERIES@'
alpine_version='@M2IMAGE_DISTRO_VERSION@'
rootfs_size='512M'
rootfs_hostname='firecrab'
# bash: Shell repository scripts often use #!/bin/bash (same as Ubuntu).
rootfs_packages='alpine-baselayout busybox bash openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt'

# Seconds since the script started, on every line — see the same helper in
# bootstrap-ubuntu-in-guest.sh for why the session log is useless for finding
# where a slow build spent its time without it.
script_started=$(date +%s)
info() { printf '[INFO +%ss] %s\n' "$(( $(date +%s) - script_started ))" "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup_mounts() {
  # MicroBoot's outer shell is busybox, whose umount applet has no -R at all
  # (found live: unrecognized option, prints usage, exits 1) — the whole call
  # silently no-ops under `2>/dev/null || true`, leaving /proc mounted live
  # under $staging when `mkfs.ext4 -d` walks it next, which then fails
  # ("No such process") trying to copy /proc's ephemeral per-process files.
  # Use the same lazy-unmount fallback as the other guest bootstrap script
  # (its own first attempt uses -R, so in practice it always lands here).
  umount "$staging/proc" 2>/dev/null || umount -l "$staging/proc" 2>/dev/null || true
  umount "$staging/sys" 2>/dev/null || umount -l "$staging/sys" 2>/dev/null || true
  umount "$staging/dev" 2>/dev/null || umount -l "$staging/dev" 2>/dev/null || true
}
trap cleanup_mounts EXIT

rm -rf "$work"
mkdir -p "$staging" "$out"

arch=$(uname -m)
case "$arch" in
  x86_64|aarch64) ;;
  *) fail "unsupported architecture: $arch" ;;
esac

info 'bringing up eth0 (MicroBoot has no network service of its own)'
# The interface exists (virtio_net loads on its own) but is administratively
# down — a real installed template's network manager brings it up as part of
# DHCP negotiation; udhcpc itself does not, so a bare `udhcpc -i eth0` on a
# down link never sends a single packet and hangs silently forever (found
# live: 0 packets captured on the host's TAP after 10+ minutes).
ip link set eth0 up || fail 'could not bring eth0 up'
udhcpc -i eth0 -n -q >/dev/null 2>&1 || fail 'could not obtain a DHCP lease on eth0'

info 'installing e2fsprogs into the outer (MicroBoot) shell'
# --initdb: this bare recovery shell was never a real Alpine install, so it
# has no /lib/apk/db at all yet (found live: "Unable to lock database: No
# such file or directory") — --initdb creates one on this root before
# installing. Not needed for the *target* rootfs below: that one is unpacked
# from Alpine's own official minirootfs archive, which already ships a
# pre-initialized apk database. curl: busybox only provides wget, not curl,
# and this script uses curl throughout (found live: "curl: not found").
apk add --no-cache --initdb --repository "${alpine_releases_base}/v${alpine_series}/main" e2fsprogs curl \
  || fail 'could not install e2fsprogs/curl into the outer shell'

branch="v${alpine_series}"
archive_file="alpine-minirootfs-${alpine_version}-${arch}.tar.gz"
checksum_file="$work/${archive_file}.sha256"
curl -fsSL "${alpine_releases_base}/${branch}/releases/${arch}/${archive_file}.sha256" \
  -o "$checksum_file" || fail 'could not download Alpine minirootfs checksum'
archive_sha256=$(awk -v file="$archive_file" '$2 == file || $2 == "*" file { print $1; exit }' "$checksum_file")
[ -n "$archive_sha256" ] || fail 'could not parse Alpine minirootfs checksum'
info "Alpine branch ${branch}, minirootfs version ${alpine_version}"

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

# Must be in place before the chroot apk install below — chroot does not
# share /etc/resolv.conf the way the /proc,/sys,/dev bind mounts do, so
# without this the extracted minirootfs's own (unusable) resolv.conf makes
# every package lookup fail DNS resolution (found live: "DNS: transient
# error", every package "no such package").
#
# Taken from the outer shell rather than written as a literal. This used to
# pin "nameserver 172.30.0.1" — the gateway of the *default* MicroNetwork —
# so a builder VM placed on any other MicroNetwork (172.33.0.0/24, say) sent
# every lookup to an address nothing answers on and failed with exactly the
# symptom above, while the outer shell, whose resolver udhcpc had just set
# from the lease, was fetching from the same mirror successfully two steps
# earlier. What the *finished image* ships is a separate concern, written
# further down, the way bootstrap-rocky-in-guest.sh already separates them.
if [ -s /etc/resolv.conf ]; then
  cp /etc/resolv.conf "${staging}/etc/resolv.conf"
else
  build_resolver=$(ip route show default 2>/dev/null | awk '{ print $3; exit }')
  [ -n "$build_resolver" ] \
    || fail 'no resolver in the outer shell and no default gateway to derive one from'
  printf 'nameserver %s\n' "$build_resolver" >"${staging}/etc/resolv.conf"
fi

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

# The resolver the finished image ships with, replacing the builder VM's own
# (whose MicroNetwork the image will usually not be on). Only ever read
# before the first DHCP lease lands: dnsmasq hands the bridge out as the
# guest's DNS server and dhcpcd rewrites this file from the lease on boot.
# Same value and same reason as bootstrap-rocky-in-guest.sh's.
rm -f "${staging}/etc/resolv.conf"
cat >"${staging}/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF
cat >"${staging}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
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
# -O ^orphan_file: this mkfs.ext4 belongs to the outer MicroBoot shell
# (Alpine 3.24's e2fsprogs 1.47.x), which enables orphan_file by default —
# but the image it writes is read back by consumers whose e2fsprogs may be
# older, and orphan_file only exists from 1.47.0 (2023). The host is one of
# those consumers on *every single VM start*, not just the guest's own boot:
# `rootfs::prepare_rootfs` runs `e2fsck -f -y` before `resize2fs`, and
# `specialize_guest` runs `e2fsck -p`. On a host still shipping e2fsprogs
# e2fsprogs 1.46.5 hosts (for example Ubuntu 22.04) every VM
# made from this template would fail to start. Building templates on the
# host never hit this because host mkfs and host e2fsck matched; sourcing
# the builder from MicroBoot is what split those two versions apart.
mkfs.ext4 -F -O '^orphan_file' -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel/initrd files) directly onto the
# real block device as its own ext4 filesystem, so the host's
# debugfs-based dump (`rootfs::dump_from_image`) can read them back at
# their root (e.g. /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4)
# after this VM is stopped.
info "publishing $out onto /dev/vda"
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# extract-vmlinux ships alongside this script in the repo but does not
# exist inside the guest — the raw vmlinuz is dumped out as-is
# (vmlinuz-virt-raw) and the host runs extract-vmlinux after pulling it
# off the guest disk, since the host is already known to have every
# decompressor it might need (same reasoning install-alpine-rootfs.sh's
# own extract_kernel already used).
info 'bootstrap complete'
