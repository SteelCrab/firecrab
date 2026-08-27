#!/usr/bin/env bash
# Collect license/copyright evidence and source provenance from a built M2Image.
set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)
manifest=${M2IMAGE_MANIFEST:-${repo_dir}/packaging/m2images.json}
image_root=${IMAGE_ROOT:-${FIRECRAB_IMAGE_ROOT:-${repo_dir}/images}}
compliance_root=${M2IMAGE_COMPLIANCE_DIR:-${image_root}/compliance}
alias=${1:-}
architecture=${2:-}

fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }
require_command() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required"; }

if [ -z "$alias" ] || [ -z "$architecture" ]; then
  fail 'usage: collect-m2image-compliance.sh <alias> <x86_64|aarch64>'
fi
case "$architecture" in x86_64|aarch64) ;; *) fail "unsupported architecture: $architecture" ;; esac
for command in python3 debugfs mktemp rm mkdir; do require_command "$command"; done

python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" validate >/dev/null
rootfs_rel=$(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
  field "$alias" "artifacts.${architecture}.rootfs")
rootfs="${image_root}/${rootfs_rel}"
sbom="${compliance_root}/${alias}-${architecture}.spdx.json"
bundle="${compliance_root}/${alias}-${architecture}"
[ -f "$rootfs" ] || fail "rootfs not found: $rootfs"
[ -s "$sbom" ] || fail "M2Image SBOM not found: $sbom"
[ -s "${repo_dir}/licenses/GPL-2.0-only.txt" ] \
  || fail "canonical GPL-2.0 text not found: ${repo_dir}/licenses/GPL-2.0-only.txt"

extract_root=$(mktemp -d "${compliance_root}/.legal.XXXXXX")
trap 'rm -rf -- "$extract_root"' EXIT
mkdir -p "$extract_root/usr/share"

# debugfs reads ext4 without mounting it. rdump reports a non-zero status when
# a distribution omits one of these conventional directories; that is expected
# and the Python collector simply records what actually exists in the image.
for guest_dir in /usr/share/licenses /usr/share/common-licenses /usr/share/spdx /usr/share/doc; do
  host_parent="${extract_root}$(dirname -- "$guest_dir")"
  mkdir -p "$host_parent"
  debugfs -R "rdump ${guest_dir} ${host_parent}" "$rootfs" >/dev/null 2>&1 || true
done

python3 "${script_dir}/m2image_compliance.py" \
  --sbom "$sbom" \
  --legal-root "$extract_root" \
  --gpl2-text "${repo_dir}/licenses/GPL-2.0-only.txt" \
  --output-dir "$bundle"

# Freeze the exact package-to-source publication contract into the same
# compliance directory that is copied into the M2Image archive. The separate
# materializer consumes this plan later; packaging never has to rediscover
# package metadata from a moving distribution repository.
python3 "${script_dir}/m2image_source_publication.py" plan \
  --source-map "$bundle/source-map.json" \
  --output "$bundle/source-publication-plan.json"

[ -s "$bundle/bundle.json" ] || fail "compliance bundle metadata missing: $bundle/bundle.json"
[ -s "$bundle/source-map.json" ] || fail "source map missing: $bundle/source-map.json"
[ -s "$bundle/source-publication-plan.json" ] \
  || fail "source publication plan missing: $bundle/source-publication-plan.json"
[ -s "$bundle/licenses/index.json" ] || fail "license index missing: $bundle/licenses/index.json"
[ -s "$bundle/licenses/GPL-2.0-only.txt" ] || fail 'GPL-2.0 text missing from M2Image compliance bundle'
