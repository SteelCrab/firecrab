#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

alias=alpine-3.24.1
arch=x86_64
compliance="$tmp/compliance"
dist="$tmp/dist"
materialized="$tmp/materialized"
mkdir -p "$compliance/${alias}-${arch}" "$dist/$arch" "$materialized/sources"

cat >"$tmp/source-map.json" <<'EOF_MAP'
{
  "schemaVersion": 1,
  "image": {
    "alias": "alpine-3.24.1",
    "version": "3.24.1",
    "distribution": "alpine",
    "architecture": "x86_64"
  },
  "packages": [
    {
      "binaryPackage": "busybox",
      "binaryVersion": "1.37.0-r31",
      "architecture": "x86_64",
      "declaredLicense": "GPL-2.0-only",
      "source": {
        "type": "alpine-aports",
        "sourcePackage": "busybox",
        "sourceVersion": "1.37.0-r31",
        "repositoryCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      }
    },
    {
      "binaryPackage": "busybox-binsh",
      "binaryVersion": "1.37.0-r31",
      "architecture": "x86_64",
      "declaredLicense": "GPL-2.0-only",
      "source": {
        "type": "alpine-aports",
        "sourcePackage": "busybox",
        "sourceVersion": "1.37.0-r31",
        "repositoryCommit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      }
    }
  ]
}
EOF_MAP

plan="$compliance/${alias}-${arch}/source-publication-plan.json"
python3 "$root/scripts/m2image_source_publication.py" plan \
  --source-map "$tmp/source-map.json" --output "$plan"
cp "$plan" "$materialized/source-publication-plan.json"
source_id=$(python3 - "$plan" <<'PY'
import json
import sys

with open(sys.argv[1], encoding='utf-8') as stream:
    plan = json.load(stream)
if plan.get('packageCount') != 2:
    raise SystemExit(f"expected packageCount=2, got {plan.get('packageCount')!r}")
if plan.get('sourceCount') != 1:
    raise SystemExit(f"expected sourceCount=1, got {plan.get('sourceCount')!r}")
sources = plan.get('sources')
if not isinstance(sources, list) or len(sources) != 1:
    raise SystemExit('expected exactly one source unit')
source_id = sources[0].get('sourceId') if isinstance(sources[0], dict) else None
if not isinstance(source_id, str) or not source_id:
    raise SystemExit('source unit is missing sourceId')
print(source_id)
PY
)
mkdir -p "$materialized/sources/$source_id"
printf 'pkgname=busybox\npkgver=1.37.0\n' >"$materialized/sources/$source_id/APKBUILD"
printf 'exact upstream source bytes\n' >"$materialized/sources/$source_id/busybox-1.37.0.tar.bz2"
python3 "$root/scripts/m2image_source_publication.py" index \
  --plan "$plan" --source-root "$materialized/sources" \
  --output "$materialized/source-index.json"

# A binary sibling is already present in a real release assembly; make sure the
# checksum contract covers it alongside the corresponding-source archive.
printf 'synthetic m2image package\n' >"$dist/$arch/${alias}.tar.zst"

M2IMAGE_COMPLIANCE_DIR="$compliance" DIST_DIR="$dist" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2image-sources.sh" \
  --alias "$alias" --arch "$arch" --materialized-dir "$materialized"

archive="$dist/$arch/${alias}.sources.tar.zst"
test -s "$archive"
zstd -dc "$archive" | tar -tf - >"$tmp/members"
grep -qx 'source-publication-plan.json' "$tmp/members"
grep -qx 'source-index.json' "$tmp/members"
grep -qx "sources/$source_id/APKBUILD" "$tmp/members"
grep -qx "sources/$source_id/busybox-1.37.0.tar.bz2" "$tmp/members"
(
  cd "$dist/$arch"
  sha256sum -c SHA256SUMS
  grep -q "${alias}\.tar\.zst" SHA256SUMS
  grep -q "${alias}\.sources\.tar\.zst" SHA256SUMS
)

# Tampering after indexing must fail closed rather than silently packaging a
# stale source-index.json.
printf 'tampered\n' >>"$materialized/sources/$source_id/APKBUILD"
if M2IMAGE_COMPLIANCE_DIR="$compliance" DIST_DIR="$tmp/tampered-dist" ZSTD_LEVEL=1 ZSTD_THREADS=1 \
  "$root/scripts/package-m2image-sources.sh" \
  --alias "$alias" --arch "$arch" --materialized-dir "$materialized" \
  >"$tmp/tampered.out" 2>&1; then
  echo 'source packaging unexpectedly accepted a stale source index' >&2
  exit 1
fi
grep -q 'materialized source index is stale or does not match source bytes' "$tmp/tampered.out"

echo 'M2Image exact source package contract passed'
