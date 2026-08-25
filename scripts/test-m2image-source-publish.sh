#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

dist="$tmp/dist"
mapfile -t aliases < <(python3 "$root/scripts/m2image-manifest.py" aliases)
mapfile -t architectures < <(python3 "$root/scripts/m2image-manifest.py" architectures)
expected_images=$((${#aliases[@]} * ${#architectures[@]}))

for arch in "${architectures[@]}"; do
  mkdir -p "$dist/$arch"
  for alias in "${aliases[@]}"; do
    printf 'binary %s %s\n' "$alias" "$arch" >"$dist/$arch/${alias}.tar.zst"
    printf 'source %s %s\n' "$alias" "$arch" >"$dist/$arch/${alias}.sources.tar.zst"
  done
  (
    cd "$dist/$arch"
    : >SHA256SUMS
    for alias in "${aliases[@]}"; do
      sha256sum "${alias}.tar.zst" "${alias}.sources.tar.zst" >>SHA256SUMS
    done
  )
done

DIST_DIR="$dist" "$root/scripts/publish-m2images-r2.sh" \
  --bucket firecrab-test --backend rclone --dry-run >"$tmp/publish.out"

# Every immutable binary must have an immutable source sibling in the dry-run.
for arch in "${architectures[@]}"; do
  for alias in "${aliases[@]}"; do
    binary_key=$(python3 "$root/scripts/m2image-manifest.py" registry-key "$alias" "$arch")
    source_key=$(python3 "$root/scripts/m2image-manifest.py" source-registry-key "$alias" "$arch")
    grep -Fq "${source_key}" "$tmp/publish.out"
    grep -Fq "${binary_key}" "$tmp/publish.out"
  done
done

# The strict catalog binds both hashes and refuses a missing source sibling.
python3 "$root/scripts/m2image-manifest.py" catalog \
  --dist-dir "$dist" --output "$tmp/catalog.json"
python3 - "$tmp/catalog.json" "$expected_images" <<'PY'
import json, sys
catalog = json.load(open(sys.argv[1], encoding='utf-8'))
expected = int(sys.argv[2])
assert len(catalog['images']) == expected
for image in catalog['images']:
    assert image['source'].endswith('.sources.tar.zst')
    assert len(image['sourceSha256']) == 64
    assert image['sourceSizeBytes'] > 0
    assert len(image['sha256']) == 64
PY

catalog_line=$(grep -nF '[INFO] publishing catalog last' "$tmp/publish.out" | tail -n1 | cut -d: -f1 || true)
last_source_line=$(grep -nF '[INFO] uploading exact source ' "$tmp/publish.out" | tail -n1 | cut -d: -f1 || true)
last_binary_line=$(grep -nE '^\[INFO\] uploading [^ ]+/[^ ]+ -> r2://' "$tmp/publish.out" | tail -n1 | cut -d: -f1 || true)
for value in "$catalog_line" "$last_source_line" "$last_binary_line"; do
  test -n "$value" || { echo 'expected publication ordering log line is missing' >&2; exit 1; }
done
test "$catalog_line" -gt "$last_source_line"
test "$catalog_line" -gt "$last_binary_line"

rm -f "$dist/x86_64/${aliases[0]}.sources.tar.zst"
if python3 "$root/scripts/m2image-manifest.py" catalog \
  --dist-dir "$dist" --output "$tmp/should-not-exist.json" \
  >"$tmp/missing.out" 2>&1; then
  echo 'catalog unexpectedly accepted a missing source archive' >&2
  exit 1
fi
grep -q 'source package not found' "$tmp/missing.out"

echo 'M2Image source publication contract passed'
