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
      mkdir -p "$stage/etc" "$stage/usr/share/licenses/busybox" "$stage/usr/share/doc/curl"
      printf 'synthetic\n' >"$stage/etc/os-release"
      printf 'busybox license\n' >"$stage/usr/share/licenses/busybox/COPYING"
      printf 'curl copyright\n' >"$stage/usr/share/doc/curl/copyright"
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
c:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

P:linux-virt
V:6.15.4-r0
A:x86_64
L:GPL-2.0-only
o:linux-lts
c:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
EOF_APK
SOURCE_DATE_EPOCH=0 python3 "$root/scripts/m2image_sbom.py" \
  --format alpine --distribution alpine --image-alias "$alias" \
  --image-version 3.24.1 --architecture "$arch" \
  --package-db "$tmp/apk-installed" \
  --output "$image_root/compliance/${alias}-${arch}.spdx.json"

M2IMAGE_COMPLIANCE_DIR="$image_root/compliance" IMAGE_ROOT="$image_root" \
  bash "$root/scripts/collect-m2image-compliance.sh" "$alias" "$arch"

plan="$image_root/compliance/${alias}-${arch}/source-publication-plan.json"
python3 - "$plan" <<'PY_PLAN'
import json
import sys

with open(sys.argv[1], encoding='utf-8') as stream:
    plan = json.load(stream)
expected = {
    'coveragePolicy': 'all-installed-packages',
    'packageCount': 2,
    'sourceBackedPackageCount': 2,
    'nonSourcePackageCount': 0,
    'sourceCount': 2,
}
for field, value in expected.items():
    if plan.get(field) != value:
        raise SystemExit(
            f"source publication plan {field} mismatch: expected {value!r}, got {plan.get(field)!r}"
        )
PY_PLAN

IMAGE_ROOT="$image_root" OUT_DIR="$out" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2images.sh" --alias "$alias" --arch "$arch"
zstd -dc "$out/${alias}.tar.zst" | tar -tf - >"$tmp/members"
grep -qx 'compliance/sbom.spdx.json' "$tmp/members"
grep -qx 'compliance/bundle.json' "$tmp/members"
grep -qx 'compliance/source-map.json' "$tmp/members"
grep -qx 'compliance/source-publication-plan.json' "$tmp/members"
grep -qx 'compliance/licenses/index.json' "$tmp/members"
grep -qx 'compliance/licenses/GPL-2.0-only.txt' "$tmp/members"
grep -qx 'compliance/licenses/guest/usr/share/licenses/busybox/COPYING' "$tmp/members"
grep -qx 'compliance/licenses/guest/usr/share/doc/curl/copyright' "$tmp/members"

rm -f "$image_root/compliance/${alias}-${arch}/source-publication-plan.json"
if IMAGE_ROOT="$image_root" OUT_DIR="$tmp/no-source-plan" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2images.sh" --alias "$alias" --arch "$arch" \
  >"$tmp/no-source-plan.out" 2>&1; then
  echo 'packaging unexpectedly succeeded without the source publication plan' >&2
  exit 1
fi
grep -q 'missing M2Image compliance artifact: .*source-publication-plan.json' "$tmp/no-source-plan.out"

rm -rf "$image_root/compliance/${alias}-${arch}"
if IMAGE_ROOT="$image_root" OUT_DIR="$tmp/no-sbom" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2images.sh" --alias "$alias" --arch "$arch" \
  >"$tmp/no-sbom.out" 2>&1; then
  echo 'packaging unexpectedly succeeded without an M2Image SBOM' >&2
  exit 1
fi
grep -q 'missing M2Image compliance bundle' "$tmp/no-sbom.out"
echo 'M2Image package compliance contract passed'
