#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
image_root="$tmp/images"
out="$tmp/out"
mkdir -p "$image_root" "$out" "$image_root/compliance"
alias=alpine-3.24.1
arch=x86_64

while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  mkdir -p "$image_root/$(dirname -- "$rel")"
  case "$rel" in
    rootfs/*)
      stage="$tmp/root-stage"
      mkdir -p "$stage/etc"
      printf 'synthetic\n' >"$stage/etc/os-release"
      truncate -s 16M "$image_root/$rel"
      mkfs.ext4 -q -F -d "$stage" "$image_root/$rel"
      ;;
    *) printf 'synthetic artifact\n' >"$image_root/$rel" ;;
  esac
done < <(python3 "$root/scripts/m2image-manifest.py" artifacts "$alias" "$arch")

cat >"$tmp/apk-installed" <<'EOF_APK'
P:busybox
V:1.37.0-r18
A:x86_64
L:GPL-2.0-only
o:busybox

P:linux-virt
V:6.15.4-r0
A:x86_64
L:GPL-2.0-only
o:linux-lts
EOF_APK
SOURCE_DATE_EPOCH=0 python3 "$root/scripts/m2image_sbom.py" \
  --format alpine --distribution alpine --image-alias "$alias" \
  --image-version 3.24.1 --architecture "$arch" \
  --package-db "$tmp/apk-installed" \
  --output "$image_root/compliance/${alias}-${arch}.spdx.json"

IMAGE_ROOT="$image_root" OUT_DIR="$out" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2images.sh" --alias "$alias" --arch "$arch"
zstd -dc "$out/${alias}.tar.zst" | tar -tf - >"$tmp/members"
grep -qx 'compliance/sbom.spdx.json' "$tmp/members"

rm "$image_root/compliance/${alias}-${arch}.spdx.json"
if IMAGE_ROOT="$image_root" OUT_DIR="$tmp/no-sbom" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2images.sh" --alias "$alias" --arch "$arch" \
  >"$tmp/no-sbom.out" 2>&1; then
  echo 'packaging unexpectedly succeeded without an M2Image SBOM' >&2
  exit 1
fi
grep -q 'missing M2Image SBOM' "$tmp/no-sbom.out"
echo 'M2Image package compliance contract passed'
