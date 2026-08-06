#!/usr/bin/env bash
# Pack known M2Image templates from a local image root into
# `{alias}.tar.zst` archives for GitHub Releases (flat asset names).
#
# Layout inside each archive matches TemplateSpec relative paths:
#   kernel/...  rootfs/...
#
# Usage:
#   ./scripts/package-m2images.sh              # alpine + ubuntu + rocky
#   ./scripts/package-m2images.sh --alias alpine-3.24
#   IMAGE_ROOT=/var/lib/firecrab/images OUT_DIR=dist/m2images ./scripts/package-m2images.sh
#
# Publish (manual — do not run from automation without review):
#   gh release create v0.1.0 \
#     --title "v0.1.0" \
#     dist/m2images/*.tar.zst dist/m2images/SHA256SUMS

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH= cd -- "${script_dir}/.." && pwd -P)

IMAGE_ROOT=${IMAGE_ROOT:-${FIRECRAB_IMAGE_ROOT:-$repo_dir/images}}
OUT_DIR=${OUT_DIR:-$repo_dir/dist/m2images}
ZSTD_LEVEL=${ZSTD_LEVEL:-19}
ALIAS_FILTER=all

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage ;;
    --alias)
      [ $# -ge 2 ] || { echo "missing value for --alias" >&2; exit 2; }
      ALIAS_FILTER=$2
      shift 2
      ;;
    --alias=*)
      ALIAS_FILTER=${1#--alias=}
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

command -v tar >/dev/null 2>&1 || fail "tar is required"
command -v zstd >/dev/null 2>&1 || fail "zstd is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"

[ -d "$IMAGE_ROOT" ] || fail "image root not found: $IMAGE_ROOT"

# Print relative artifact paths for a known alias (one per line).
# Keep in sync with firecrab-api/src/templates.rs default_specs().
artifacts_for() {
  case "$1" in
    alpine-3.24)
      printf '%s\n' \
        kernel/vmlinux-alpine-virt-x86_64 \
        kernel/initramfs-alpine-virt-x86_64 \
        rootfs/alpine-rootfs-3.24.1-x86_64.ext4
      ;;
    ubuntu-26.04)
      printf '%s\n' \
        kernel/vmlinux-ubuntu-26.04-x86_64 \
        rootfs/ubuntu-rootfs-26.04-amd64.ext4
      ;;
    rocky-9)
      printf '%s\n' \
        kernel/vmlinux-rocky-9-x86_64 \
        kernel/initramfs-rocky-9-x86_64 \
        rootfs/rocky-rootfs-9-x86_64.ext4
      ;;
    *)
      fail "unknown alias: $1 (want alpine-3.24, ubuntu-26.04, or rocky-9)"
      ;;
  esac
}

aliases_to_pack() {
  case "$ALIAS_FILTER" in
    all) printf '%s\n' alpine-3.24 ubuntu-26.04 rocky-9 ;;
    alpine-3.24|ubuntu-26.04|rocky-9) printf '%s\n' "$ALIAS_FILTER" ;;
    *) fail "unknown --alias $ALIAS_FILTER" ;;
  esac
}

package_one() {
  local alias=$1
  local out=$OUT_DIR/${alias}.tar.zst
  local -a files=()
  local rel

  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    [ -f "$IMAGE_ROOT/$rel" ] || fail "missing $IMAGE_ROOT/$rel (build the template first)"
    files+=("$rel")
  done < <(artifacts_for "$alias")

  info "packing $alias → $out"
  # --sparse keeps large ext4 images from ballooning when mostly free space.
  # Pipe through zstd so the GitHub asset stays under the 2 GiB limit.
  tar --sparse -C "$IMAGE_ROOT" -cf - "${files[@]}" \
    | zstd -T0 -"$ZSTD_LEVEL" -f -o "$out"

  local bytes
  bytes=$(wc -c <"$out" | tr -d ' ')
  info "  $alias: $bytes bytes ($(numfmt --to=iec-i --suffix=B "$bytes" 2>/dev/null || echo "$bytes B"))"
}

mkdir -p "$OUT_DIR"

packed=0
while IFS= read -r alias; do
  package_one "$alias"
  packed=$((packed + 1))
done < <(aliases_to_pack)

info "writing $OUT_DIR/SHA256SUMS"
(
  cd "$OUT_DIR"
  # All archives present (partial --alias runs must not drop other checksums).
  : >SHA256SUMS
  shopt -s nullglob
  for archive in *.tar.zst; do
    sha256sum "$archive" >>SHA256SUMS
  done
)

info "done ($packed archive(s) in $OUT_DIR)"
info "publish example:"
info "  gh release create v0.1.0 \\"
info "    --title 'v0.1.0' \\"
info "    $OUT_DIR/*.tar.zst $OUT_DIR/SHA256SUMS"
info "then set FIRECRAB_IMAGE_BASE_URL=https://github.com/<org>/<repo>/releases/download/v0.1.0"
