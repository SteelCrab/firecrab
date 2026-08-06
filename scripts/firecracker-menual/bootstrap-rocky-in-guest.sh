#!/bin/sh
# Runs entirely inside a firecrab builder VM that is itself already
# rocky-9 (enforced by handlers::bootstrap::requires_matching_source,
# since this needs dnf already present in the outer guest — unlike
# Alpine/Ubuntu, Rocky has no minimal base tarball with its own bundled
# package manager to chroot into first). Adapted from
# install-rocky-rootfs.sh's write_configure_script, minus the outer
# `docker run --cap-add=SYS_ADMIN ...` wrapper — a real microVM guest
# already has mount/chroot natively, no capability grant needed.
set -eu

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
rootfs_size='2G'
rootfs_hostname='firecrab'
baseos_url='https://download.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/'
appstream_url='https://download.rockylinux.org/pub/rocky/9/AppStream/x86_64/os/'
rootfs_packages='kernel dracut systemd systemd-udev NetworkManager iproute iputils bind-utils curl ca-certificates procps-ng openssh-server kmod util-linux dhcp-client e2fsprogs'

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

chroot_mounts=''
cleanup_chroot_mounts() {
  for target in $chroot_mounts; do
    umount -R "$target" 2>/dev/null || umount -l "$target" 2>/dev/null || true
  done
  chroot_mounts=''
}
trap cleanup_chroot_mounts EXIT

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

rm -rf "$work"
mkdir -p "$staging/etc/pki" "$staging/dev" "$staging/proc" "$staging/sys" "$staging/run" "$out"
cp -a /etc/pki/rpm-gpg "$staging/etc/pki/"

dnf_common="--disablerepo=* --enablerepo=baseos,appstream --setopt=baseos.mirrorlist= --setopt=baseos.baseurl=${baseos_url} --setopt=appstream.mirrorlist= --setopt=appstream.baseurl=${appstream_url} --setopt=install_weak_deps=False --setopt=keepcache=False"

info 'installing Rocky Linux 9 guest packages into the staging root'
mount_chroot_fs
# package/flag lists are deliberate whitespace lists.
# shellcheck disable=SC2086
dnf -q -y --installroot="$staging" --releasever=9 --setopt=reposdir=/etc/yum.repos.d \
  $dnf_common install $rootfs_packages

rm -rf "$staging/var/cache/dnf" "$staging/var/log/dnf"* "$staging/var/cache/yum" "$staging/var/log/yum"* 2>/dev/null || true

cat >"$staging/etc/hostname" <<EOF
${rootfs_hostname}
EOF
cat >"$staging/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 ${rootfs_hostname}
EOF
cat >"$staging/etc/fstab" <<'EOF'
/dev/vda / ext4 defaults 0 1
EOF
: >"$staging/etc/machine-id"
rm -f "$staging/etc/resolv.conf"
cat >"$staging/etc/resolv.conf" <<'EOF'
nameserver 172.30.0.1
EOF

install -d -m 0755 "$staging/etc/NetworkManager/system-connections"
cat >"$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection" <<'EOF'
[connection]
id=firecrab-ethernet
type=ethernet
autoconnect=true

[ipv4]
method=auto
may-fail=false

[ipv6]
method=disabled
EOF
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

cat >"$staging/etc/systemd/system/serial-getty@ttyS0.service.d/autologin.conf" <<'EOF'
[Unit]
BindsTo=
After=
After=systemd-user-sessions.service getty-pre.target

[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear --keep-baud 115200,57600,38400,9600 %I $TERM
EOF

install -d -m 0755 "$staging/usr/local/sbin"
cat >"$staging/usr/local/sbin/firecrab-network-ready.sh" <<'EOF'
#!/bin/sh
set -eu
ipv4=""
for _ in $(seq 1 15); do
    ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
        awk '$2 != "lo" { split($4, address, "/"); print address[1]; exit }')
    [ -n "$ipv4" ] && break
    sleep 1
done
if [ -z "$ipv4" ]; then
    echo "FIRECRAB_NETWORK_FAILED no-ipv4-address"
elif getent hosts example.com >/dev/null 2>&1; then
    echo "FIRECRAB_NETWORK_READY $ipv4"
else
    echo "FIRECRAB_NETWORK_FAILED dns-unreachable"
fi
EOF
chmod 0755 "$staging/usr/local/sbin/firecrab-network-ready.sh"

cat >"$staging/etc/systemd/system/firecrab-network-ready.service" <<'EOF'
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
EOF
ln -sfn /etc/systemd/system/firecrab-network-ready.service \
  "$staging/etc/systemd/system/multi-user.target.wants/firecrab-network-ready.service"

# EL9's kernel-install layout keeps the raw kernel under
# /usr/lib/modules/<version>/vmlinuz (no separate /boot/vmlinuz-* copy).
vmlinuz_path=$(find "$staging/usr/lib/modules" -mindepth 2 -maxdepth 2 -type f -name vmlinuz | sort -V | tail -n1)
[ -n "$vmlinuz_path" ] || fail 'Rocky kernel package did not install usr/lib/modules/*/vmlinuz'
kernel_version=$(basename "$(dirname "$vmlinuz_path")")
initrd_path="$staging/boot/initramfs-${kernel_version}.img"
kernel_config="$staging/usr/lib/modules/${kernel_version}/config"

grep -Eq '^CONFIG_VIRTIO_PCI=(y|m)$' "$kernel_config" || fail "Rocky kernel lacks CONFIG_VIRTIO_PCI: ${kernel_config}"

info "building generic dracut initramfs for ${kernel_version}"
chroot "$staging" /usr/bin/dracut --force --no-hostonly \
  --add-drivers 'virtio_blk virtio_pci virtio_net ext4' \
  "/boot/initramfs-${kernel_version}.img" "$kernel_version"
cleanup_chroot_mounts

[ -s "$initrd_path" ] || fail "dracut did not create ${initrd_path}"
test -e "$staging/etc/os-release" || fail 'missing /etc/os-release'
test -e "$staging/sbin/init" || fail 'missing /sbin/init'
test -x "$staging/usr/sbin/sshd" || fail 'missing sshd'
network_profile="$staging/etc/NetworkManager/system-connections/firecrab-ethernet.nmconnection"
test -e "$network_profile" || fail 'missing transport-independent DHCP profile'
if grep -q '^interface-name=' "$network_profile"; then
  fail 'Rocky DHCP profile must not pin a Firecracker transport-specific interface name'
fi
test -e "$staging/etc/systemd/system/firecrab-network-ready.service" || fail 'missing network readiness service'

cp "$vmlinuz_path" "$out/vmlinuz-raw"
cp "$initrd_path" "$out/initramfs"

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Everything under $out is read back off this VM's *block device* by the
# host (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so
# the guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent file and
# packages it as if it were a complete rootfs.
sync

# Same reasoning as the Alpine/Ubuntu scripts: extract-vmlinux runs on the
# HOST (Task 8) against $out/vmlinuz-raw once it's dumped out of this
# guest's own disk.
info 'bootstrap complete'
