#!/usr/bin/env bash
# Pack known M2Image templates from a local image root into
# `{alias}.tar.zst` archives for a distribution/version package registry.
#
# Layout inside each archive matches TemplateSpec relative paths:
#   kernel/...  rootfs/...
#
# Usage:
#   ./scripts/package-m2images.sh              # alpine + ubuntu + rocky
#   ./scripts/package-m2images.sh --alias alpine-3.24
#   IMAGE_ROOT=/var/lib/firecrab/images OUT_DIR=dist/m2images ./scripts/package-m2images.sh
#   ZSTD_THREADS=4 ./scripts/package-m2images.sh # override the safe 2-thread default
#
# Publish (manual — do not run from automation without review):
#   ./scripts/publish-m2images.sh --alias ubuntu-26.04

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)

IMAGE_ROOT=${IMAGE_ROOT:-${FIRECRAB_IMAGE_ROOT:-$repo_dir/images}}
OUT_DIR=${OUT_DIR:-$repo_dir/dist/m2images}
ZSTD_LEVEL=${ZSTD_LEVEL:-19}
ZSTD_THREADS=${ZSTD_THREADS:-2}
ALIAS_FILTER=all
MOTD_FILE=${MOTD_FILE:-$repo_dir/assets/firecrab-motd}

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

for command in tar zstd sha256sum debugfs; do
  command -v "$command" >/dev/null 2>&1 || fail "$command is required"
done

[ -d "$IMAGE_ROOT" ] || fail "image root not found: $IMAGE_ROOT"
[ -f "$MOTD_FILE" ] || fail "MOTD file not found: $MOTD_FILE"
case "$ZSTD_THREADS" in
  ''|*[!0-9]*) fail "ZSTD_THREADS must be a non-negative integer" ;;
esac

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
  local staging=$PACK_WORK_DIR/$alias
  local -a files=()
  local rel
  local rootfs_rel=

  mkdir -p "$staging"

  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    [ -f "$IMAGE_ROOT/$rel" ] || fail "missing $IMAGE_ROOT/$rel (build the template first)"
    files+=("$rel")
    case "$rel" in rootfs/*) rootfs_rel=$rel ;; esac
    mkdir -p "$staging/$(dirname -- "$rel")"
    cp --reflink=auto --sparse=always -- "$IMAGE_ROOT/$rel" "$staging/$rel"
  done < <(artifacts_for "$alias")

  [ -n "$rootfs_rel" ] || fail "no rootfs artifact configured for $alias"
  cp -- "$MOTD_FILE" "$staging/.firecrab-motd"
  (
    cd "$staging"
    debugfs -w -R 'rm /etc/motd' "$rootfs_rel" >/dev/null 2>&1 || true
    motd_output=$(debugfs -w -R 'write .firecrab-motd /etc/motd' "$rootfs_rel" 2>&1)
    case "$motd_output" in
      *'Allocated inode'*) ;;
      *) fail "could not install MOTD into $alias rootfs: $motd_output" ;;
    esac
  )
  rm -f -- "$staging/.firecrab-motd"

  info "packing $alias → $out"
  # --sparse keeps large ext4 images from ballooning when mostly free space.
  # Keep compression from starving running VMs; 0 remains an explicit opt-in
  # to zstd's all-core mode through ZSTD_THREADS=0.
  tar --sparse -C "$staging" -cf - "${files[@]}" \
    | zstd -T"$ZSTD_THREADS" -"$ZSTD_LEVEL" -f -o "$out"

  local bytes
  bytes=$(wc -c <"$out" | tr -d ' ')
  info "  $alias: $bytes bytes ($(numfmt --to=iec-i --suffix=B "$bytes" 2>/dev/null || echo "$bytes B"))"
}

mkdir -p "$OUT_DIR"
PACK_WORK_DIR=$(mktemp -d "$OUT_DIR/.package.XXXXXX")
trap 'rm -rf -- "$PACK_WORK_DIR"' EXIT

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
info "  ./scripts/publish-m2images.sh --alias <alias>"
info "after publishing, set FIRECRAB_IMAGE_BASE_URL=https://registry.firecrab.dev"
