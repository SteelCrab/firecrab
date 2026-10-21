#!/usr/bin/env bash
# Build and package Firecracker M2Images from packaging/m2images.json.

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)
manifest=${M2IMAGE_MANIFEST:-${repo_dir}/packaging/m2images.json}
dist_dir=${DIST_DIR:-${repo_dir}/dist/m2images}
out_dir=${OUT_DIR:-}
compliance_dir=${M2IMAGE_COMPLIANCE_DIR:-${repo_dir}/images/compliance}
alias_filter=all
architecture=${M2IMAGE_ARCH:-}
package_images=1

usage() {
  cat <<'EOF'
Usage: ./scripts/build-m2images.sh [options]

Options:
  --alias <alias|all>         Build one manifest alias or all aliases (default: all)
  --arch <x86_64|aarch64>    Target architecture (default: uname -m)
  --manifest <path>          Alternate release manifest
  --dist-dir <path>          Package root (default: dist/m2images)
  --no-package               Build image files without packaging them
  --list                     List configured aliases and exit
  -h, --help                 Show this help

The manifest pins distribution versions, builders, artifact paths, internal
revisions, and Cloudflare R2 object keys. Build each architecture on a native
host of that architecture; package outputs are written below <dist-dir>/<arch>.
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
    --no-package) package_images=0; shift ;;
    --list)
      require_command python3
      python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" aliases
      exit 0
      ;;
    *) fail "unknown argument: $1" ;;
  esac
done

require_command python3
require_command sha256sum
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" validate >/dev/null
architecture=$(normalize_architecture "${architecture:-$(uname -m)}")
export M2IMAGE_ARCH=$architecture
mkdir -p "$compliance_dir"

if [ -z "$out_dir" ]; then
  out_dir="${dist_dir}/${architecture}"
fi

mapfile -t known_aliases < <(
  python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" aliases
)
selected_aliases=()
if [ "$alias_filter" = all ]; then
  selected_aliases=("${known_aliases[@]}")
else
  for alias in "${known_aliases[@]}"; do
    if [ "$alias" = "$alias_filter" ]; then
      selected_aliases=("$alias")
      break
    fi
  done
  [ "${#selected_aliases[@]}" -eq 1 ] || fail "unknown --alias: $alias_filter"
fi

for alias in "${selected_aliases[@]}"; do
  builder=$(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
    field "$alias" builder.script)
  version=$(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
    field "$alias" version)
  [ -x "${repo_dir}/${builder}" ] || fail "builder is not executable: ${repo_dir}/${builder}"

  while IFS= read -r command; do
    [ -n "$command" ] && require_command "$command"
  done < <(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" requires "$alias")

  builder_environment=()
  while IFS=$'\t' read -r key value; do
    [ -n "$key" ] || continue
    builder_environment+=("${key}=${value}")
  done < <(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" environment "$alias")

  sbom_output="${compliance_dir}/${alias}-${architecture}.spdx.json"
  rm -f -- "$sbom_output"
  info "building ${alias} (distribution ${version}, ${architecture})"
  env "M2IMAGE_ALIAS=${alias}" "M2IMAGE_SBOM_OUTPUT=${sbom_output}" \
    "${builder_environment[@]}" "${repo_dir}/${builder}"
  [ -s "$sbom_output" ] || fail "builder did not produce M2Image SBOM: $sbom_output"
  python3 - "$sbom_output" "$alias" <<'PY_VALIDATE'
import json
import sys

path, expected_alias = sys.argv[1:3]
try:
    with open(path, encoding="utf-8") as stream:
        doc = json.load(stream)
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"invalid M2Image SBOM {path}: {exc}") from exc

if doc.get("spdxVersion") != "SPDX-2.3":
    raise SystemExit(
        f"invalid M2Image SBOM {path}: expected SPDX-2.3, "
        f"got {doc.get('spdxVersion')!r}"
    )
packages = doc.get("packages")
if not isinstance(packages, list) or not packages:
    raise SystemExit(f"invalid M2Image SBOM {path}: packages must be a non-empty list")
actual_alias = packages[0].get("name") if isinstance(packages[0], dict) else None
if actual_alias != expected_alias:
    raise SystemExit(
        f"invalid M2Image SBOM {path}: expected image alias {expected_alias!r}, "
        f"got {actual_alias!r}"
    )
PY_VALIDATE
  M2IMAGE_MANIFEST="$manifest" IMAGE_ROOT="${repo_dir}/images" \
    M2IMAGE_COMPLIANCE_DIR="$compliance_dir" \
    bash "${script_dir}/collect-m2image-compliance.sh" "$alias" "$architecture"
  [ -s "${compliance_dir}/${alias}-${architecture}/bundle.json" ] \
    || fail "builder did not produce M2Image compliance bundle for ${alias}/${architecture}"
done

if [ "$package_images" -eq 1 ]; then
  package_alias=$alias_filter
  info "packaging ${package_alias} into ${out_dir}"
  IMAGE_ROOT="${repo_dir}/images" OUT_DIR="$out_dir" DIST_DIR="$dist_dir" \
    M2IMAGE_ARCH="$architecture" M2IMAGE_MANIFEST="$manifest" \
    "${script_dir}/package-m2images.sh" --alias "$package_alias" --arch "$architecture"

  info "verifying ${out_dir}/SHA256SUMS"
  (cd "$out_dir" && sha256sum -c SHA256SUMS)
fi

info 'M2Image build complete'
