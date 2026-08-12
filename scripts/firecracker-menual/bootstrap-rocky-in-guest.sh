#!/bin/sh
# Runs entirely inside a firecrab MicroBoot builder VM (Alpine's own
# recovery shell — see crate::microboot's doc comment), not inside an
# installed Rocky template. Rocky's dnf/rpm are glibc binaries the
# musl-based outer shell can't run directly, so this script downloads
# Rocky's own official Container-Base image and chroots into it first —
# everything from that point on (dnf --installroot into $staging) is
# unchanged from install-rocky-rootfs.sh's own write_configure_script,
# minus its outer `docker run --cap-add=SYS_ADMIN ...` wrapper.
set -eu

# The interface exists (virtio_net loads on its own) but is administratively
# down — a real installed template's network manager brings it up as part of
# DHCP negotiation; udhcpc itself does not, so a bare `udhcpc -i eth0` on a
# down link never sends a single packet and hangs silently forever (found
# live: 0 packets captured on the host's TAP after 10+ minutes). Written
# without the info/fail helpers since they aren't defined yet at this point.
ip link set eth0 up || {
  printf '[FAIL] %s\n' 'could not bring eth0 up' >&2
  exit 1
}
udhcpc -i eth0 -n -q >/dev/null 2>&1 || {
  printf '[FAIL] %s\n' 'could not obtain a DHCP lease on eth0' >&2
  exit 1
}

work=/root/fc-bootstrap
staging="$work/staging"
out="$work/out"
rootfs_size='2G'
rootfs_hostname='firecrab'
rocky_release='@M2IMAGE_DISTRO_VERSION@'
rocky_repository_base='@ROCKY_REPOSITORY_BASE@'
rocky_container_build='@ROCKY_CONTAINER_BUILD@'
case "$(uname -m)" in
  x86_64)
    rocky_arch='x86_64'
    ;;
  aarch64)
    rocky_arch='aarch64'
    ;;
  *)
    printf '[FAIL] unsupported Rocky architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac
baseos_url="${rocky_repository_base}/${rocky_release}/BaseOS/${rocky_arch}/os/"
appstream_url="${rocky_repository_base}/${rocky_release}/AppStream/${rocky_arch}/os/"
container_name="Rocky-9-Container-Base-${rocky_release}-${rocky_container_build}.${rocky_arch}.oci.tar.xz"
container_url="${rocky_repository_base}/${rocky_release}/images/${rocky_arch}/${container_name}"
# dnf must be inside the guest (see install-rocky-rootfs.sh); dashboard package
# actions invoke `dnf` on the serial console.
rootfs_packages='kernel dracut systemd systemd-udev NetworkManager iproute iputils bind-utils curl ca-certificates procps-ng openssh-server kmod util-linux dhcp-client e2fsprogs dnf'

# Seconds since the script started, on every line — see the same helper in
# bootstrap-ubuntu-in-guest.sh for why the session log is useless for finding
# where a slow build spent its time without it.
script_started=$(date +%s)
info() { printf '[INFO +%ss] %s\n' "$(( $(date +%s) - script_started ))" "$*"; }
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
  chroot_mounts="$staging/proc $chroot_mounts"
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
mkdir -p "$work"
# MicroBoot's `/` is the kernel's own initramfs `rootfs` — a ramfs, which
# keeps no size accounting at all: it doesn't even appear in `df`, and
# statvfs on it reports zero blocks total *and* zero free. rpm runs exactly
# that statvfs before every transaction, reads "0 bytes available" and
# aborts with "At least NNNMB more space needed on the / filesystem" — the
# figure is just the transaction's own size, so it stays identical no
# matter how much RAM the VM has (found live: an unchanged 350 MB shortfall
# at both 4 GiB and 8 GiB, with `free` reporting 7.4 GiB idle). Alpine's apk
# and Ubuntu's apt make no such precheck, which is why only Rocky hits it.
# A tmpfs does report real numbers, so mount one over the whole work area:
# with no size= it defaults to half the VM's RAM, which both scales with
# `handlers::bootstrap`'s builder RAM and gives rpm a truthful answer.
mount -t tmpfs tmpfs "$work" || fail 'could not mount a tmpfs work area'
mkdir -p "$staging/etc/pki" "$staging/dev" "$staging/proc" "$staging/sys" "$staging/run" "$out"

# Rocky's dnf/rpm are glibc binaries; MicroBoot's outer shell is Alpine
# (musl) and cannot run them directly (verified live: dnf/rpm exist inside
# the extracted Container-Base but exec fails from outside a matching
# libc environment). Download Rocky's own official Container-Base — the
# same artifact `docker pull rockylinux:9` resolves to — and chroot into
# IT first, so the dnf that actually runs below is Rocky's own, under its
# own glibc. This container_root is discarded once dnf finishes (it never
# becomes part of the target rootfs in $staging).
container_root="$work/container-base"
container_archive="$work/rocky-container-base.tar.xz"
mkdir -p "$container_root"

info 'installing e2fsprogs and container tooling into the outer (MicroBoot) shell'
# --initdb: this bare recovery shell was never a real Alpine install, so it
# has no /lib/apk/db at all yet (found live: "Unable to lock database: No
# such file or directory") — --initdb creates one on this root before
# installing. curl: busybox only provides wget, not curl, and this script
# uses curl throughout (found live: "curl: not found").
apk add --no-cache --initdb --repository 'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' \
  e2fsprogs jq curl \
  || fail 'could not install e2fsprogs/jq/curl into the outer shell'

info "downloading Rocky ${rocky_release} ${rocky_arch} Container-Base"
curl -fsSL "$container_url" \
  -o "$container_archive" || fail 'could not download Rocky Container-Base'
checksum_file="$work/${container_name}.CHECKSUM"
curl -fsSL "${container_url}.CHECKSUM" -o "$checksum_file" \
  || fail 'could not download Rocky Container-Base checksum'
container_sha256=$(sed -n "s/^SHA256 (${container_name}) = //p" "$checksum_file")
[ -n "$container_sha256" ] || fail 'could not parse Rocky Container-Base checksum'
printf '%s  %s\n' "$container_sha256" "$container_archive" \
  | sha256sum -c - >/dev/null \
  || fail 'Rocky Container-Base checksum verification failed'

# Rocky has published both OCI and Docker archive layouts across releases.
# Support both official layouts so a pinned distro version does not depend on
# the current release's container packaging format.
oci_dir="$work/container-oci"
mkdir -p "$oci_dir"
tar -xJf "$container_archive" -C "$oci_dir"
info "extracting Rocky ${rocky_release} ${rocky_arch} Container-Base"
if [ -f "$oci_dir/index.json" ]; then
  manifest_digest=$(jq -r '.manifests[0].digest | sub("^sha256:"; "")' "$oci_dir/index.json")
  # Negative form rather than `A && B || fail`, which reads as if-then-else
  # without being one (SC2015) and older shellcheck releases reject.
  if [ -z "$manifest_digest" ] || [ "$manifest_digest" = null ]; then
    fail 'could not read Container-Base manifest digest from index.json'
  fi
  layer_digest=$(jq -r '.layers[0].digest | sub("^sha256:"; "")' "$oci_dir/blobs/sha256/$manifest_digest")
  if [ -z "$layer_digest" ] || [ "$layer_digest" = null ]; then
    fail 'could not read Container-Base layer digest from its manifest'
  fi
  tar -xzf "$oci_dir/blobs/sha256/$layer_digest" -C "$container_root"
elif [ -f "$oci_dir/manifest.json" ]; then
  jq -r '.[0].Layers[]' "$oci_dir/manifest.json" | while IFS= read -r layer; do
    [ -f "$oci_dir/$layer" ] || fail "Container-Base layer is missing: $layer"
    tar -xf "$oci_dir/$layer" -C "$container_root"
  done
else
  fail 'Container-Base is neither an OCI nor Docker archive'
fi
test -x "$container_root/usr/bin/rpm" || fail 'Container-Base is missing usr/bin/rpm'
test -e "$container_root/usr/bin/dnf" || fail 'Container-Base is missing usr/bin/dnf'

# Rocky's own RPM signing keys, which the repo configs below reference as
# gpgkey=file:///etc/pki/rpm-gpg/... inside $staging. Under an installed
# Rocky builder template these came from the outer shell's own
# /etc/pki/rpm-gpg, but MicroBoot's outer shell is Alpine and has no such
# directory (found live: "cp: can't stat '/etc/pki/rpm-gpg'") — Rocky's
# Container-Base ships the identical keys, so take them from there.
test -d "$container_root/etc/pki/rpm-gpg" || fail 'Container-Base is missing etc/pki/rpm-gpg'
cp -a "$container_root/etc/pki/rpm-gpg" "$staging/etc/pki/"

# Everything under $work lives in the MicroBoot guest's RAM-backed root
# (tmpfs, half of the VM's RAM) — unlike an installed builder template,
# where it was a real disk. The downloaded archive and the unpacked OCI
# layout are both dead weight once $container_root exists, and dnf below
# needs every megabyte it can get (found live: rocky's install died with
# "At least 350MB more space needed on the / filesystem").
rm -rf "$oci_dir" "$container_archive"

mount -t proc proc "$container_root/proc"
mount --rbind /sys "$container_root/sys"
mount --make-rslave "$container_root/sys"
mount --rbind /dev "$container_root/dev"
mount --make-rslave "$container_root/dev"
cp /etc/resolv.conf "$container_root/etc/resolv.conf" 2>/dev/null || true
chroot_mounts="$container_root/proc $container_root/sys $container_root/dev $chroot_mounts"

dnf_common="--disablerepo=* --enablerepo=baseos,appstream --setopt=baseos.mirrorlist= --setopt=baseos.baseurl=${baseos_url} --setopt=appstream.mirrorlist= --setopt=appstream.baseurl=${appstream_url} --setopt=install_weak_deps=False --setopt=keepcache=False"

info "installing Rocky Linux ${rocky_release} ${rocky_arch} guest packages into the staging root"
mount_chroot_fs
# Bind the in-progress target root and dnf's repo config into the
# Container-Base chroot so `chroot "$container_root" dnf --installroot=...`
# can see and populate $staging exactly as a native `dnf` invocation would.
mkdir -p "$container_root$staging" "$container_root/etc/yum.repos.d"
mount --rbind "$staging" "$container_root$staging"
mount --make-rslave "$container_root$staging"
chroot_mounts="$container_root$staging $chroot_mounts"
# package/flag lists are deliberate whitespace lists.
# shellcheck disable=SC2086
chroot "$container_root" dnf -q -y --installroot="$staging" --releasever="$rocky_release" \
  --setopt=reposdir=/etc/yum.repos.d $dnf_common install $rootfs_packages

# Fixed version-pinned baseurls — stock mirrorlist needs $rltype, which this image lacks.
cat >"$staging/etc/yum.repos.d/rocky-firecrab.repo" <<EOF
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
EOF
if [ -f "$staging/etc/yum.repos.d/rocky.repo" ]; then
  sed -i 's/^enabled=1/enabled=0/' "$staging/etc/yum.repos.d/rocky.repo"
fi
test -x "$staging/usr/bin/dnf" || test -x "$staging/bin/dnf" || \
  fail 'Rocky rootfs is missing /usr/bin/dnf after package install'

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
install -d -m 0755 "$staging/etc/modules-load.d"
cat >"$staging/etc/modules-load.d/firecrab-network.conf" <<'EOF'
virtio_net
EOF
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

info "building generic dracut initramfs for ${kernel_version}"
chroot "$staging" /usr/bin/dracut --force --no-hostonly \
  --add-drivers "$virtio_drivers" \
  "/boot/initramfs-${kernel_version}.img" "$kernel_version"
cleanup_chroot_mounts

[ -s "$initrd_path" ] || fail "dracut did not create ${initrd_path}"
test -e "$staging/etc/os-release" || fail 'missing /etc/os-release'
installed_release=$(sed -n 's/^VERSION_ID=//p' "$staging/etc/os-release" | tr -d '"')
[ "$installed_release" = "$rocky_release" ] \
  || fail "Rocky rootfs is not pinned to VERSION_ID ${rocky_release}"
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

# Now that it is safely in $out, drop it from the tree that becomes the
# rootfs. Firecracker loads the initrd the host hands it (the copy just
# made), never one from inside the guest's own filesystem, so the in-rootfs
# copy is 27.9 MB — 7.5% of this image, measured with debugfs on a built
# one — that is written twice by the two mkfs.ext4 passes below, shipped in
# every package, and never read. EL9 owns this path as %ghost precisely
# because it is regenerated rather than shipped, so dracut inside a running
# guest recreates it on the next kernel update exactly as it would have.
rm -f "$initrd_path"

info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
# -O ^orphan_file: see the identical flag in the alpine/ubuntu scripts —
# this mkfs.ext4 is the outer MicroBoot shell's (Alpine 3.24, e2fsprogs
# 1.47.x, which enables orphan_file by default), while the images it writes
# are fsck'd by whatever e2fsprogs the *consumers* have. Rocky is the
# loudest case: its own initramfs runs e2fsprogs 1.46.5 (2021), so
# systemd-fsck-root fails and the guest drops into a dracut emergency shell
# without ever reaching /sysroot — found live while verifying Task 8 of the
# MicroBoot plan, then reproduced on the host by dumping that very e2fsck
# out of the produced image and running it ("has unsupported feature(s):
# orphan_file / Get a newer version of e2fsck!", clean once it's off).
mkfs.ext4 -F -O '^orphan_file' -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel/initrd files) directly onto the
# real block device as its own ext4 filesystem, so the host's
# debugfs-based dump (`rootfs::dump_from_image`) can read them back at
# their root (e.g. /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4)
# after this VM is stopped.
info "publishing ${out} onto /dev/vda"
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# Same reasoning as the Alpine/Ubuntu scripts: extract-vmlinux runs on the
# HOST against vmlinuz-raw once it's been dumped off this guest's own
# disk.
info 'bootstrap complete'
