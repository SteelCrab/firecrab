#!/usr/bin/env bash
# Publish a complete M2Image release to a Cloudflare R2-backed registry.

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)
manifest=${M2IMAGE_MANIFEST:-${repo_dir}/packaging/m2images.json}
dist_dir=${DIST_DIR:-${repo_dir}/dist/m2images}
bucket=${R2_BUCKET:-}
rclone_remote=${R2_REMOTE:-r2}
backend=${R2_BACKEND:-auto}
dry_run=0

usage() {
  cat <<'EOF'
Usage: ./scripts/publish-m2images-r2.sh --bucket <bucket> [options]

Options:
  --bucket <name>             Cloudflare R2 bucket (or R2_BUCKET)
  --backend <auto|rclone|wrangler>
                              Upload client (default: auto; prefers rclone)
  --rclone-remote <name>      rclone remote configured for R2 (default: r2)
  --manifest <path>           Alternate release manifest
  --dist-dir <path>           Package root (default: dist/m2images)
  --dry-run                   Validate and print uploads without changing R2
  -h, --help                  Show this help

The publisher requires every manifest alias for x86_64 and aarch64, including
its exact corresponding-source sibling archive. Source archives and M2Images
are uploaded before catalog.json, so consumers never discover a release whose
source bytes are absent. Configure rclone's S3 provider as Cloudflare for large
artifacts. Wrangler supports only files up to 315 MB and is a fallback.
EOF
}

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }
require_command() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required"; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --bucket) [ "$#" -ge 2 ] || fail 'missing value for --bucket'; bucket=$2; shift 2 ;;
    --bucket=*) bucket=${1#--bucket=}; shift ;;
    --backend) [ "$#" -ge 2 ] || fail 'missing value for --backend'; backend=$2; shift 2 ;;
    --backend=*) backend=${1#--backend=}; shift ;;
    --rclone-remote) [ "$#" -ge 2 ] || fail 'missing value for --rclone-remote'; rclone_remote=$2; shift 2 ;;
    --rclone-remote=*) rclone_remote=${1#--rclone-remote=}; shift ;;
    --manifest) [ "$#" -ge 2 ] || fail 'missing value for --manifest'; manifest=$2; shift 2 ;;
    --manifest=*) manifest=${1#--manifest=}; shift ;;
    --dist-dir) [ "$#" -ge 2 ] || fail 'missing value for --dist-dir'; dist_dir=$2; shift 2 ;;
    --dist-dir=*) dist_dir=${1#--dist-dir=}; shift ;;
    --dry-run) dry_run=1; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$bucket" ] || fail '--bucket or R2_BUCKET is required'
case "$bucket" in *[!a-z0-9.-]*|'') fail "invalid R2 bucket name: $bucket" ;; esac
case "$backend" in auto|rclone|wrangler) ;; *) fail "unsupported backend: $backend" ;; esac
require_command python3
require_command sha256sum
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" validate >/dev/null

if [ "$backend" = auto ]; then
  if command -v rclone >/dev/null 2>&1; then backend=rclone
  elif command -v wrangler >/dev/null 2>&1; then backend=wrangler
  elif [ "$dry_run" -eq 1 ]; then backend=rclone
  else fail 'install rclone (recommended) or Wrangler v4'
  fi
fi
if [ "$dry_run" -eq 0 ]; then require_command "$backend"; fi
if [ "$backend" = wrangler ] && [ "$dry_run" -eq 0 ]; then
  wrangler_version=$(wrangler --version 2>/dev/null | sed -nE 's/.* ([0-9]+)\..*/\1/p' | head -n1)
  if [ -z "$wrangler_version" ] || [ "$wrangler_version" -lt 4 ]; then
    fail 'Wrangler v4 or newer is required'
  fi
fi

mapfile -t aliases < <(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" aliases)
mapfile -t architectures < <(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" architectures)

for architecture in "${architectures[@]}"; do
  checksum_file="${dist_dir}/${architecture}/SHA256SUMS"
  [ -f "$checksum_file" ] || fail "checksum file not found: $checksum_file"
  (cd "${dist_dir}/${architecture}" && sha256sum -c SHA256SUMS)
done

# Catalog construction is a fail-closed completeness check: it now requires the
# binary and source sibling for every alias/architecture and binds both hashes.
catalog_tmp=$(mktemp "${dist_dir}/.catalog.XXXXXX")
trap 'rm -f -- "$catalog_tmp"' EXIT
python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" catalog \
  --dist-dir "$dist_dir" --output "$catalog_tmp"

print_command() {
  printf '[DRY-RUN]'
  printf ' %q' "$@"
  printf '\n'
}

upload() {
  local source=$1
  local key=$2
  local content_type=$3
  local content_disposition=
  [ -f "$source" ] || fail "upload source not found: $source"

  if [ "$content_type" = application/zstd ]; then
    content_disposition="attachment; filename=\"$(basename -- "$key")\""
  fi

  if [ "$backend" = rclone ]; then
    command=(rclone copyto "$source" "${rclone_remote}:${bucket}/${key}")
    [ -z "$content_disposition" ] \
      || command+=(--metadata-set "content-disposition=${content_disposition}")
    [ "$dry_run" -eq 0 ] || command+=(--dry-run)
  else
    bytes=$(wc -c <"$source" | tr -d ' ')
    if [ "$bytes" -gt 315000000 ]; then
      fail "Wrangler cannot upload ${source} (${bytes} bytes, limit 315 MB); use --backend rclone"
    fi
    command=(wrangler r2 object put "${bucket}/${key}" --file "$source" --remote \
      --content-type "$content_type")
    [ -z "$content_disposition" ] \
      || command+=(--content-disposition "$content_disposition")
    [ "$content_type" != application/zstd ] || command+=(--cache-control 'public, max-age=31536000, immutable')
  fi

  if [ "$dry_run" -eq 1 ]; then print_command "${command[@]}"
  else "${command[@]}"
  fi
}

for alias in "${aliases[@]}"; do
  for architecture in "${architectures[@]}"; do
    source_package="${dist_dir}/${architecture}/${alias}.sources.tar.zst"
    source_key=$(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
      source-registry-key "$alias" "$architecture")
    info "uploading exact source ${alias}/${architecture} -> r2://${bucket}/${source_key}"
    upload "$source_package" "$source_key" application/zstd

    package="${dist_dir}/${architecture}/${alias}.tar.zst"
    key=$(python3 "${script_dir}/m2image-manifest.py" --manifest "$manifest" \
      registry-key "$alias" "$architecture")
    info "uploading ${alias}/${architecture} -> r2://${bucket}/${key}"
    upload "$package" "$key" application/zstd
  done
done

catalog_key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["registry"]["catalogKey"])' "$manifest")
info "publishing catalog last -> r2://${bucket}/${catalog_key}"
upload "$catalog_tmp" "$catalog_key" application/json

info "R2 publication complete (${backend})"
