#!/bin/sh
# Runs entirely inside a firecrab MicroBoot builder VM — Alpine's own
# recovery shell (see crate::microboot's doc comment), NOT an arbitrary
# installed template: this script hard-depends on that environment via
# `apk add --initdb`, `udhcpc`, and busybox's umount quirks below.
# Downloads the official Ubuntu Base tarball, chroots in and installs
# packages/kernel via ITS OWN apt (not the outer guest's), then packs the
# result into an ext4 image via `mkfs.ext4 -d` (no loop mount). Adapted
# from install-ubuntu-roofs.sh, minus its host-sudo re-exec and
# ownership-restore steps — the guest is already root and disposable.
set -eu

work=/root/fc-bootstrap
mount_dir="$work/staging"
out="$work/out"
ubuntu_base_url='https://cdimage.ubuntu.com/ubuntu-base/releases'
series='@M2IMAGE_DISTRO_SERIES@'
rootfs_size='2G'
rootfs_hostname='firecrab'
rootfs_packages='systemd systemd-sysv systemd-resolved udev kmod util-linux linux-image-generic iproute2 iputils-ping net-tools dnsutils curl ca-certificates procps openssh-server'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup_mounts() {
  # MicroBoot's outer shell is busybox, whose umount applet has no -R at all
  # (found live: unrecognized option, prints usage, exits 1) — the whole call
  # silently no-ops under `2>/dev/null || true`, leaving /proc mounted live
  # under $mount_dir when `mkfs.ext4 -d` walks it next, which then fails
  # ("No such process") trying to copy /proc's ephemeral per-process files.
  # Same lazy-umount fallback bootstrap-rocky-in-guest.sh's cleanup relies on
  # (its own first attempt uses -R, so in practice it always lands here).
  umount "$mount_dir/proc" 2>/dev/null || umount -l "$mount_dir/proc" 2>/dev/null || true
  umount "$mount_dir/sys" 2>/dev/null || umount -l "$mount_dir/sys" 2>/dev/null || true
  umount "$mount_dir/dev" 2>/dev/null || umount -l "$mount_dir/dev" 2>/dev/null || true
}
trap cleanup_mounts EXIT

rm -rf "$work"
mkdir -p "$mount_dir" "$out"

arch=amd64
case "$(uname -m)" in
  x86_64) arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
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
# installing. curl: busybox only provides wget, not curl, and this script
# uses curl throughout (found live: "curl: not found").
apk add --no-cache --initdb --repository 'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' e2fsprogs curl \
  || fail 'could not install e2fsprogs/curl into the outer shell'

release_url="${ubuntu_base_url}/${series}/release"
index_html="$work/index.html"
curl -fsSL "${release_url}/" -o "$index_html" || fail 'could not download Ubuntu Base release index'
archive_name=$(grep -Eo "ubuntu-base-[0-9.]+-base-${arch}\\.tar\\.gz" "$index_html" | sort -V | tail -n1)
[ -n "$archive_name" ] || fail "could not find an Ubuntu Base archive for ${series}/${arch}"

archive_path="$work/${archive_name}"
curl -fsSL "${release_url}/${archive_name}" -o "$archive_path" || fail 'could not download the Ubuntu Base archive'

checksum_file="$work/SHA256SUMS"
curl -fsSL "${release_url}/SHA256SUMS" -o "$checksum_file" || fail 'could not download Ubuntu Base checksums'
checksum_line=$(grep -E "[ *]${archive_name}\$" "$checksum_file") || fail 'checksum entry not found'
(cd "$work" && printf '%s\n' "$checksum_line" | sha256sum -c -) || fail 'Ubuntu Base checksum mismatch'

info 'extracting Ubuntu Base'
tar --numeric-owner -xpf "$archive_path" -C "$mount_dir"

cat >"${mount_dir}/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"${mount_dir}/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"${mount_dir}/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
: >"${mount_dir}/etc/machine-id"

install -d -m 0755 "${mount_dir}/etc/systemd/network"
cat >"${mount_dir}/etc/systemd/network/10-eth0.network" <<'EOF'
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

install -d -m 0755 "${mount_dir}/usr/local/sbin"
cat >"${mount_dir}/usr/local/sbin/firecrab-network-ready.sh" <<'EOF'
#!/bin/sh
set -eu
# network-online does not mean DHCP finished or systemd-resolved's stub is up.
# Poll for IPv4, repair a dangling resolved stub, then accept getent or dig@gw.
ipv4=""
for _ in $(seq 1 15); do
  ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
    awk '$2 != "lo" { split($4, a, "/"); print a[1]; exit }')
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
EOF
chmod 0755 "${mount_dir}/usr/local/sbin/firecrab-network-ready.sh"
cat >"${mount_dir}/etc/systemd/system/firecrab-network-ready.service" <<'EOF'
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

install -d -m 0755 "${mount_dir}/etc/systemd/system/getty.target.wants"
ln -sf /lib/systemd/system/serial-getty@.service \
  "${mount_dir}/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"
install -d -m 0755 "${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d"
cat >"${mount_dir}/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

install -d -m 0755 "${mount_dir}/dev" "${mount_dir}/proc" "${mount_dir}/sys" "${mount_dir}/run"
install -d -m 1777 "${mount_dir}/tmp"

cp /etc/resolv.conf "${mount_dir}/etc/resolv.conf"
mount -t proc proc "${mount_dir}/proc"
mount --rbind /sys "${mount_dir}/sys"
mount --rbind /dev "${mount_dir}/dev"

info "installing packages: ${rootfs_packages}"
chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get update
# package list is a deliberate whitespace list.
# shellcheck disable=SC2086
chroot "$mount_dir" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $rootfs_packages
chroot "$mount_dir" apt-get clean
rm -rf "${mount_dir}/var/lib/apt/lists/"*

vmlinuz_path=$(find "${mount_dir}/boot" -maxdepth 1 -name 'vmlinuz-*' | sort -V | tail -n1)
[ -n "$vmlinuz_path" ] || fail 'linux-image-generic did not install a vmlinuz'
cp "$vmlinuz_path" "$out/vmlinuz-raw"

# Enable systemd-resolved only when the package actually shipped a unit.
# Enabling the wants symlink without the package leaves resolv.conf pointing
# at a stub that never appears (dns-unreachable despite working host DNS).
resolved_unit=""
for candidate in \
  "${mount_dir}/lib/systemd/system/systemd-resolved.service" \
  "${mount_dir}/usr/lib/systemd/system/systemd-resolved.service"
do
  if [ -e "$candidate" ]; then
    resolved_unit=$candidate
    break
  fi
done
if [ -n "$resolved_unit" ]; then
  ln -sf /lib/systemd/system/systemd-resolved.service \
    "${mount_dir}/etc/systemd/system/multi-user.target.wants/systemd-resolved.service"
  ln -sfn /run/systemd/resolve/stub-resolv.conf "${mount_dir}/etc/resolv.conf"
else
  # Fallback: readiness script will rewrite resolv.conf to the DHCP gateway.
  rm -f "${mount_dir}/etc/systemd/system/multi-user.target.wants/systemd-resolved.service"
  printf 'nameserver 1.1.1.1\n' >"${mount_dir}/etc/resolv.conf"
fi

cleanup_mounts

test -e "${mount_dir}/etc/os-release" || fail 'missing /etc/os-release'
test -e "${mount_dir}/sbin/init" || fail 'missing /sbin/init'
test -e "${mount_dir}/usr/sbin/sshd" || fail 'missing sshd'

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
# -O ^orphan_file: same reasoning as the alpine/rocky scripts — the outer
# MicroBoot shell's mkfs.ext4 (Alpine 3.24, e2fsprogs 1.47.x) enables it by
# default, but the host re-checks this image with its own e2fsck on every
# VM start (`prepare_rootfs`'s `e2fsck -f -y`, `specialize_guest`'s
# `e2fsck -p`), and hosts still on e2fsprogs 1.46.5 don't know the feature.
mkfs.ext4 -F -O '^orphan_file' -L rootfs -d "$mount_dir" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel file) directly onto the real
# block device as its own ext4 filesystem, so the host's debugfs-based
# dump (`rootfs::dump_from_image`) can read them back at their root (e.g.
# /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4) after this VM is
# stopped.
info "publishing $out onto /dev/vda"
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# Same reasoning as the Alpine script: extract-vmlinux runs on the HOST
# against vmlinuz-raw once it's been dumped off this guest's own disk —
# not in here.
info 'bootstrap complete'
