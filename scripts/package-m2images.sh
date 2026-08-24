#!/usr/bin/env bash
# Package manifest-defined M2Image artifacts as sparse tar.zst archives.

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)

manifest=${M2IMAGE_MANIFEST:-${repo_dir}/packaging/m2images.json}
image_root=${IMAGE_ROOT:-${FIRECRAB_IMAGE_ROOT:-${repo_dir}/images}}
compliance_root=${M2IMAGE_COMPLIANCE_DIR:-${image_root}/compliance}
dist_dir=${DIST_DIR:-${repo_dir}/dist/m2images}
out_dir=${OUT_DIR:-}
architecture=${M2IMAGE_ARCH:-}
alias_filter=all
zstd_level=${ZSTD_LEVEL:-19}
zstd_threads=${ZSTD_THREADS:-2}
motd_file=${MOTD_FILE:-${repo_dir}/assets/firecrab-motd}

usage() {
  cat <<'EOF'
Usage: ./scripts/package-m2images.sh [options]

Options:
  --alias <alias|all>         Package one manifest alias or all aliases
  --arch <x86_64|aarch64>    Artifact architecture (default: uname -m)
  --manifest <path>          Alternate release manifest
  --dist-dir <path>          Package root (default: dist/m2images)
  -h, --help                 Show this help

Output is <dist-dir>/<arch>/{alias}.tar.zst plus SHA256SUMS. Set OUT_DIR to
override only the architecture output directory.
EOF
}

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }
require_command() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required"; }

normalize_architecture() {
  case "$1" in
    x86_64|amd64) printf '%s\n' x86_64 ;;
    aarch64|arm64) printf '%s\n' aarch64 ;;
    *) fail "unsupported architecture: $1 (want x86_64 or aarch64)" ;;
  esac
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --alias) [ "$#" -ge 2 ] || fail 'missing value for --alias'; alias_filter=$2; shift 2 ;;
    --alias=*) alias_filter=${1#--alias=}; shift ;;
    --arch) [ "$#" -ge 2 ] || fail 'missing value for --arch'; architecture=$2; shift 2 ;;
    --arch=*) architecture=${1#--arch=}; shift ;;
    --manifest) [ "$#" -ge 2 ] || fail 'missing value for --manifest'; manifest=$2; shift 2 ;;
    --manifest=*) manifest=${1#--manifest=}; shift ;;
    --dist-dir) [ "$#" -ge 2 ] || fail 'missing value for --dist-dir'; dist_dir=$2; shift 2 ;;
    --dist-dir=*) dist_dir=${1#--dist-dir=}; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

for command in python3 tar zstd sha256sum debugfs cp; do require_command "$command"; done
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" validate >/dev/null
architecture=$(normalize_architecture "${architecture:-$(uname -m)}")
[ -n "$out_dir" ] || out_dir="${dist_dir}/${architecture}"
[ -d "$image_root" ] || fail "image root not found: $image_root"
[ -f "$motd_file" ] || fail "MOTD file not found: $motd_file"
case "$zstd_threads" in ''|*[!0-9]*) fail 'ZSTD_THREADS must be a non-negative integer' ;; esac
case "$zstd_level" in ''|*[!0-9]*) fail 'ZSTD_LEVEL must be a non-negative integer' ;; esac

mapfile -t known_aliases < <(
  python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" aliases
)
selected_aliases=()
if [ "$alias_filter" = all ]; then
  selected_aliases=("${known_aliases[@]}")
else
  for alias in "${known_aliases[@]}"; do
    [ "$alias" = "$alias_filter" ] && selected_aliases=("$alias")
  done
  [ "${#selected_aliases[@]}" -eq 1 ] || fail "unknown --alias: $alias_filter"
fi

mkdir -p "$out_dir"
package_work_dir=$(mktemp -d "${out_dir}/.package.XXXXXX")
trap 'rm -rf -- "$package_work_dir"' EXIT

package_one() {
  local alias=$1
  local output="${out_dir}/${alias}.tar.zst"
  local staging="${package_work_dir}/${alias}"
  local rootfs_rel=''
  local rel
  local -a files=()
  mkdir -p "$staging"

  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    [ -f "${image_root}/${rel}" ] || fail "missing ${image_root}/${rel} (build ${alias} first)"
    files+=("$rel")
    case "$rel" in rootfs/*) rootfs_rel=$rel ;; esac
    mkdir -p "$staging/$(dirname -- "$rel")"
    cp --reflink=auto --sparse=always -- "${image_root}/${rel}" "$staging/$rel"
  done < <(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
    artifacts "$alias" "$architecture")

  [ -n "$rootfs_rel" ] || fail "no rootfs artifact configured for $alias/$architecture"

  local compliance_source="${compliance_root}/${alias}-${architecture}"
  [ -d "$compliance_source" ] \
    || fail "missing M2Image compliance bundle: $compliance_source (build $alias first)"
  for required in bundle.json source-map.json source-publication-plan.json \
    sbom.spdx.json README.txt licenses/index.json licenses/GPL-2.0-only.txt; do
    [ -s "${compliance_source}/${required}" ] \
      || fail "missing M2Image compliance artifact: ${compliance_source}/${required}"
  done
  python3 - "${compliance_source}/sbom.spdx.json" "$alias" <<'PY_SBOM'
import json, sys
doc = json.load(open(sys.argv[1], encoding='utf-8'))
assert doc.get('spdxVersion') == 'SPDX-2.3', 'M2Image SBOM is not SPDX 2.3'
assert doc.get('packages', [{}])[0].get('name') == sys.argv[2], 'M2Image SBOM alias mismatch'
PY_SBOM
  mkdir -p "$staging/compliance"
  cp -a -- "$compliance_source/." "$staging/compliance/"
  files+=("compliance")

  cp -- "$motd_file" "$staging/.firecrab-motd"
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

  info "packing $alias/$architecture -> $output"
  tar --sparse -C "$staging" -cf - "${files[@]}" \
    | zstd -T"$zstd_threads" -"$zstd_level" -f -o "$output"
  bytes=$(wc -c <"$output" | tr -d ' ')
  info "  $alias: $bytes bytes ($(numfmt --to=iec-i --suffix=B "$bytes" 2>/dev/null || printf '%s B' "$bytes"))"
}

for alias in "${selected_aliases[@]}"; do package_one "$alias"; done

info "writing ${out_dir}/SHA256SUMS"
(
  cd "$out_dir"
  : >SHA256SUMS
  # Ignore stale archives from aliases removed from the current manifest.
  # Source archives are sibling release artifacts and must survive a binary
  # repack instead of silently falling out of the checksum contract.
  for alias in "${known_aliases[@]}"; do
    for suffix in '.tar.zst' '.sources.tar.zst'; do
      candidate="${alias}${suffix}"
      [ ! -f "$candidate" ] || sha256sum "$candidate" >>SHA256SUMS
    done
  done
)

# A complete cross-architecture catalog is generated only when every manifest
# package is present. The publisher performs the strict generation before R2
# upload; this best-effort copy is useful while assembling artifacts locally.
if python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" catalog \
  --dist-dir "$dist_dir" --output "${dist_dir}/catalog.json" 2>/dev/null; then
  info "updated ${dist_dir}/catalog.json"
else
  info 'catalog deferred until all x86_64 and aarch64 packages are present'
fi

info "done (${#selected_aliases[@]} archive(s) in $out_dir)"
