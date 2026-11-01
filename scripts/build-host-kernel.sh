#!/usr/bin/env bash
# The caller verifies the source archive before starting this native build.
set -euo pipefail

check_only=0
if [ "$#" -eq 2 ] && [ "$1" = --check ]; then
  check_only=1
  architecture=$2
elif [ "$#" -eq 5 ]; then
  architecture=$1
  version=$2
  archive=$3
  work=$4
  output=$5
else
  printf 'Usage: %s --check <architecture> | <architecture> <version> <source.tar.xz> <work-directory> <output>\n' "$0" >&2
  exit 2
fi

[ "$(uname -s)" = Linux ] || { printf 'Kernel builds require a Linux host\n' >&2; exit 1; }
case "$(uname -m):$architecture" in
  x86_64:x86_64) make_arch=x86; target=vmlinux; image=vmlinux ;;
  aarch64:aarch64|arm64:aarch64) make_arch=arm64; target=Image; image=arch/arm64/boot/Image ;;
  *) printf 'Kernel builds require a matching host architecture: %s\n' "$architecture" >&2; exit 1 ;;
esac
for tool in make cc flex bison bc perl tar xz install grep; do
  command -v "$tool" >/dev/null || { printf 'Missing kernel build tool: %s\n' "$tool" >&2; exit 1; }
done
if ! printf '#include <libelf.h>\n#include <gelf.h>\n#include <openssl/opensslv.h>\n' | cc -x c -fsyntax-only - >/dev/null 2>&1; then
  printf 'Missing kernel build headers: install libelf and OpenSSL development packages\n' >&2
  exit 1
fi
jobs=${FIRECRAB_KERNEL_BUILD_JOBS:-2}
[[ "$jobs" =~ ^[1-9][0-9]*$ ]] || { printf 'FIRECRAB_KERNEL_BUILD_JOBS must be a positive integer\n' >&2; exit 1; }
[ "$check_only" -eq 0 ] || exit 0

# Each attempt has a fresh directory; an interrupted build is never reused.
mkdir -p "$work/source" "$work/objects"
work=$(cd -- "$work" && pwd -P)
tar -xJf "$archive" --no-same-owner --no-same-permissions -C "$work/source"
source="$work/source/linux-$version"
objects="$work/objects"
args=(-C "$source" O="$objects" ARCH="$make_arch")
make "${args[@]}" defconfig
"$source/scripts/config" --file "$objects/.config" \
  --enable BLOCK --enable BLK_DEV --enable DEVTMPFS --enable DEVTMPFS_MOUNT \
  --enable EXT4_FS --enable NET --enable INET --enable UNIX --enable PACKET \
  --enable VIRTIO --enable VIRTIO_BLK --enable VIRTIO_NET \
  --enable VIRTIO_MMIO --enable VIRTIO_MMIO_CMDLINE_DEVICES \
  --enable VSOCKETS --enable VIRTIO_VSOCKETS \
  --enable TTY --enable SERIAL_8250 --enable SERIAL_8250_CONSOLE \
  --enable UNIX98_PTYS --enable BINFMT_ELF --enable BINFMT_SCRIPT \
  --enable PROC_FS --enable SYSFS --enable TMPFS \
  --disable MODULES --disable DEBUG_INFO --disable DEBUG_INFO_BTF \
  --enable DEBUG_INFO_NONE --disable LOCALVERSION_AUTO \
  --set-str LOCALVERSION '-firecrab-host1' \
  --set-str SYSTEM_TRUSTED_KEYS '' --set-str SYSTEM_REVOCATION_KEYS ''
if [ "$architecture" = aarch64 ]; then
  "$source/scripts/config" --file "$objects/.config" --enable ARM64_4K_PAGES --disable ARM64_16K_PAGES --disable ARM64_64K_PAGES --enable EFI --enable EFI_STUB
fi
make "${args[@]}" olddefconfig
required=(BLOCK DEVTMPFS DEVTMPFS_MOUNT EXT4_FS NET INET UNIX PACKET VIRTIO_BLK VIRTIO_NET VIRTIO_MMIO VIRTIO_MMIO_CMDLINE_DEVICES VSOCKETS VIRTIO_VSOCKETS SERIAL_8250_CONSOLE UNIX98_PTYS BINFMT_ELF BINFMT_SCRIPT PROC_FS SYSFS TMPFS)
if [ "$architecture" = aarch64 ]; then required+=(ARM64_4K_PAGES EFI_STUB); fi
for option in "${required[@]}"; do
  grep -qx "CONFIG_$option=y" "$objects/.config" || { printf 'Missing built-in kernel option: %s\n' "$option" >&2; exit 1; }
done
make "${args[@]}" -j"$jobs" "$target"
install -m 0644 "$objects/$image" "$output"
install -m 0644 "$objects/.config" "$output.config"
