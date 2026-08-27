#!/usr/bin/env bash
# Materialize and package exact corresponding-source artifacts for one M2Image.

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)
manifest=${M2IMAGE_MANIFEST:-${repo_dir}/packaging/m2images.json}
compliance_root=${M2IMAGE_COMPLIANCE_DIR:-${repo_dir}/images/compliance}
dist_dir=${DIST_DIR:-${repo_dir}/dist/m2images}
alias=''
architecture=${M2IMAGE_ARCH:-}
materialized_dir=''
zstd_level=${ZSTD_LEVEL:-19}
zstd_threads=${ZSTD_THREADS:-2}

usage() {
  cat <<'EOF'
Usage: ./scripts/package-m2image-sources.sh --alias <alias> --arch <arch> [options]

Options:
  --alias <alias>             Manifest M2Image alias
  --arch <x86_64|aarch64>    Artifact architecture
  --manifest <path>          Alternate release manifest
  --dist-dir <path>          Package root (default: dist/m2images)
  --compliance-root <path>   Built compliance root (default: images/compliance)
  --materialized-dir <path>  Package an already-fetched source bundle instead
                             of fetching source bytes now
  -h, --help                 Show this help

The frozen compliance/source-publication-plan.json is authoritative. Without
--materialized-dir this command fetches every exact source unit in that plan,
hashes the bytes, and writes <dist-dir>/<arch>/<alias>.sources.tar.zst.
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
    --alias) [ "$#" -ge 2 ] || fail 'missing value for --alias'; alias=$2; shift 2 ;;
    --alias=*) alias=${1#--alias=}; shift ;;
    --arch) [ "$#" -ge 2 ] || fail 'missing value for --arch'; architecture=$2; shift 2 ;;
    --arch=*) architecture=${1#--arch=}; shift ;;
    --manifest) [ "$#" -ge 2 ] || fail 'missing value for --manifest'; manifest=$2; shift 2 ;;
    --manifest=*) manifest=${1#--manifest=}; shift ;;
    --dist-dir) [ "$#" -ge 2 ] || fail 'missing value for --dist-dir'; dist_dir=$2; shift 2 ;;
    --dist-dir=*) dist_dir=${1#--dist-dir=}; shift ;;
    --compliance-root) [ "$#" -ge 2 ] || fail 'missing value for --compliance-root'; compliance_root=$2; shift 2 ;;
    --compliance-root=*) compliance_root=${1#--compliance-root=}; shift ;;
    --materialized-dir) [ "$#" -ge 2 ] || fail 'missing value for --materialized-dir'; materialized_dir=$2; shift 2 ;;
    --materialized-dir=*) materialized_dir=${1#--materialized-dir=}; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$alias" ] || fail '--alias is required'
[ -n "$architecture" ] || fail '--arch is required'
for command in python3 tar zstd sha256sum cp cmp mktemp; do require_command "$command"; done
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" validate >/dev/null
architecture=$(normalize_architecture "$architecture")
# Also proves the alias exists and the requested architecture is manifest-backed.
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
  registry-key "$alias" "$architecture" >/dev/null

plan="${compliance_root}/${alias}-${architecture}/source-publication-plan.json"
[ -s "$plan" ] || fail "source publication plan not found: $plan"
python3 - "$plan" "$alias" "$architecture" <<'PY_PLAN'
import json
import sys

path, expected_alias, expected_arch = sys.argv[1:4]
with open(path, encoding='utf-8') as stream:
    plan = json.load(stream)
image = plan.get('image') or {}
if plan.get('schemaVersion') != 1:
    raise SystemExit('source plan schemaVersion must be 1')
if plan.get('coveragePolicy') != 'all-installed-packages':
    raise SystemExit('unsupported source coverage policy')
if image.get('alias') != expected_alias:
    raise SystemExit(
        f"source plan alias mismatch: expected {expected_alias!r}, got {image.get('alias')!r}"
    )
if image.get('architecture') != expected_arch:
    raise SystemExit(
        f"source plan architecture mismatch: expected {expected_arch!r}, got {image.get('architecture')!r}"
    )
package_count = plan.get('packageCount')
if not isinstance(package_count, int) or package_count <= 0:
    raise SystemExit('source plan has no installed packages')
PY_PLAN

out_dir="${dist_dir}/${architecture}"
mkdir -p "$out_dir"
work_dir=$(mktemp -d "${out_dir}/.sources.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
bundle="${work_dir}/bundle"

if [ -n "$materialized_dir" ]; then
  [ -d "$materialized_dir" ] || fail "materialized source bundle not found: $materialized_dir"
  mkdir -p "$bundle"
  cp -a -- "$materialized_dir/." "$bundle/"
  for required in source-publication-plan.json source-index.json; do
    [ -s "$bundle/$required" ] || fail "materialized source bundle is missing $required"
  done
  [ -d "$bundle/sources" ] || fail 'materialized source bundle is missing sources/'

  # A cached/materialized directory is accepted only if it is byte-accounted
  # against the exact frozen plan we are packaging now. This rejects stale
  # indexes and prevents a release from pairing source bytes with another build.
  python3 - "$plan" "$bundle/source-publication-plan.json" <<'PY_MATCH'
import json
import sys

with open(sys.argv[1], encoding='utf-8') as stream:
    expected = json.load(stream)
with open(sys.argv[2], encoding='utf-8') as stream:
    actual = json.load(stream)
if actual != expected:
    raise SystemExit('materialized source plan does not match frozen M2Image plan')
PY_MATCH
  python3 "${script_dir}/m2image_source_publication.py" index \
    --plan "$plan" --source-root "$bundle/sources" \
    --output "$work_dir/reindexed.json" >/dev/null
  cmp -s "$work_dir/reindexed.json" "$bundle/source-index.json" \
    || fail 'materialized source index is stale or does not match source bytes'
else
  info "materializing exact sources for ${alias}/${architecture}"
  python3 "${script_dir}/fetch_m2image_sources.py" \
    --plan "$plan" --output-dir "$bundle"
fi

for required in source-publication-plan.json source-index.json; do
  [ -s "$bundle/$required" ] || fail "source bundle is missing $required"
done
[ -d "$bundle/sources" ] || fail 'source bundle is missing sources/'

output="${out_dir}/${alias}.sources.tar.zst"
info "packing exact sources ${alias}/${architecture} -> ${output}"
tar -C "$bundle" -cf - source-publication-plan.json source-index.json sources \
  | zstd -T"$zstd_threads" -"$zstd_level" -f -o "$output"

# Regenerate the architecture checksum contract from manifest-known artifacts.
# Either package script may run last, so both binary and source archives are
# included whenever present.
mapfile -t known_aliases < <(
  python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" aliases
)
(
  cd "$out_dir"
  : >SHA256SUMS
  for known_alias in "${known_aliases[@]}"; do
    for suffix in '.tar.zst' '.sources.tar.zst'; do
      candidate="${known_alias}${suffix}"
      [ ! -f "$candidate" ] || sha256sum "$candidate" >>SHA256SUMS
    done
  done
)

bytes=$(wc -c <"$output" | tr -d ' ')
info "source package complete: ${bytes} bytes"
