#!/usr/bin/env bash
# Publish one locally packaged M2Image to the Firecrab R2 registry.
#
# Usage:
#   ./scripts/publish-m2images.sh --alias ubuntu-26.04 [--arch x86_64|aarch64]
#
# Required environment:
#   R2_ACCOUNT_ID R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_BUCKET
#
# The script is deliberately manual-only.  It uploads the package before the
# catalog, so a catalog never refers to an object that is not present yet.
#
# Registry layout:
#   catalog.json
#   ubuntu/26.04/ubuntu-26.04.tar.zst
#   ubuntu/26.04/SHA256SUMS
#   ubuntu/26.04/aarch64/ubuntu-26.04.tar.zst
#   ubuntu/26.04/aarch64/SHA256SUMS

set -euo pipefail

unset CDPATH
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(cd -- "${script_dir}/.." && pwd -P)

OUT_DIR=${OUT_DIR:-}
ALIAS=
M2IMAGE_ARCH=${M2IMAGE_ARCH:-}

usage() {
  cat <<'EOF'
Usage: ./scripts/publish-m2images.sh --alias <alias> [--arch x86_64|aarch64]

Publishes OUT_DIR/<alias>.tar.zst with a per-distribution SHA256SUMS and
updates catalog.json in the configured Cloudflare R2 bucket.

Architecture:
  --arch              Package architecture (default: uname -m)
                      ARM64 uses OUT_DIR=dist/m2images/aarch64 by default.

Required environment:
  R2_ACCOUNT_ID       Cloudflare account ID
  R2_ACCESS_KEY_ID    R2 S3 API access key ID
  R2_SECRET_ACCESS_KEY R2 S3 API secret access key
  R2_BUCKET           R2 bucket name

Optional environment:
  OUT_DIR             Package directory (x86_64: dist/m2images,
                      aarch64: dist/m2images/aarch64)
  PUBLISHED_AT        RFC 3339 timestamp for catalog.json (default: current UTC)

This is a manual publishing command. Do not run it from CI.
EOF
}

info() { printf '[INFO] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --alias)
      [ "$#" -ge 2 ] || fail 'missing value for --alias'
      ALIAS=$2
      shift 2
      ;;
    --alias=*)
      ALIAS=${1#--alias=}
      shift
      ;;
    --arch)
      [ "$#" -ge 2 ] || fail 'missing value for --arch'
      M2IMAGE_ARCH=$2
      shift 2
      ;;
    --arch=*)
      M2IMAGE_ARCH=${1#--arch=}
      shift
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[ -n "$ALIAS" ] || fail 'pass exactly one alias with --alias'
[[ "$ALIAS" =~ ^[a-z0-9][a-z0-9._-]*$ ]] || fail "invalid alias: $ALIAS"

case "${M2IMAGE_ARCH:-$(uname -m)}" in
  x86_64|amd64) M2IMAGE_ARCH=x86_64 ;;
  aarch64|arm64) M2IMAGE_ARCH=aarch64 ;;
  *) fail "unsupported architecture: ${M2IMAGE_ARCH:-$(uname -m)}" ;;
esac
[ "$M2IMAGE_ARCH" = x86_64 ] || [ "$ALIAS" != rocky-9 ] \
  || fail 'rocky-9 publishing currently supports x86_64 only'

if [ -z "$OUT_DIR" ]; then
  if [ "$M2IMAGE_ARCH" = aarch64 ]; then
    OUT_DIR="${repo_dir}/dist/m2images/aarch64"
  else
    OUT_DIR="${repo_dir}/dist/m2images"
  fi
fi

for command in aws jq tar zstd sha256sum awk date; do
  command -v "$command" >/dev/null 2>&1 || fail "$command is required"
done

for variable in R2_ACCOUNT_ID R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_BUCKET; do
  [ -n "${!variable:-}" ] || fail "$variable must be set"
done

archive="${OUT_DIR}/${ALIAS}.tar.zst"
checksums="${OUT_DIR}/SHA256SUMS"
[ -f "$archive" ] || fail "package not found: $archive"
[ -f "$checksums" ] || fail "checksum file not found: $checksums"

archive_name="${ALIAS}.tar.zst"

# Keep the remote layout human-browsable without teaching callers about
# storage keys. These aliases are the same supported by package-m2images.sh.
registry_dir_for() {
  case "$1" in
    alpine-3.24) printf '%s\n' 'alpine/3.24' ;;
    ubuntu-26.04) printf '%s\n' 'ubuntu/26.04' ;;
    rocky-9) printf '%s\n' 'rocky/9' ;;
    *) fail "unknown alias: $1 (want alpine-3.24, ubuntu-26.04, or rocky-9)" ;;
  esac
}

registry_dir=$(registry_dir_for "$ALIAS")
if [ "$M2IMAGE_ARCH" = aarch64 ]; then
  registry_dir="${registry_dir}/aarch64"
fi
package_key="${registry_dir}/${archive_name}"
checksum_key="${registry_dir}/SHA256SUMS"
archive_sha256=$(sha256sum "$archive" | awk '{print $1}')
mapfile -t local_checksum_entries < <(
  awk -v name="$archive_name" '$2 == name || $2 == "*" name { print $1 }' "$checksums"
)
[ "${#local_checksum_entries[@]}" -eq 1 ] || fail "$checksums must contain exactly one checksum for $archive_name"
[ "${local_checksum_entries[0]}" = "$archive_sha256" ] || fail "checksum mismatch for $archive_name"

# The rootfs is sparse inside the package, so calculate from tar's logical
# member size rather than the compressed archive's on-disk size.
mapfile -t rootfs_sizes < <(
  tar --list --verbose --zstd --file "$archive" \
    | awk '$NF ~ /^rootfs\// { print $3 }'
)
[ "${#rootfs_sizes[@]}" -eq 1 ] || fail "$archive must contain exactly one rootfs/ member"
[[ "${rootfs_sizes[0]}" =~ ^[0-9]+$ ]] || fail "could not read rootfs size from $archive"
min_disk_gb=$(( (rootfs_sizes[0] + 1024 * 1024 * 1024 - 1) / (1024 * 1024 * 1024) ))
[ "$min_disk_gb" -gt 0 ] || fail "rootfs size must be greater than zero"

published_at=${PUBLISHED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}
if ! date -u -d "$published_at" +%Y-%m-%dT%H:%M:%SZ >/dev/null 2>&1; then
  fail "PUBLISHED_AT must be an RFC 3339 timestamp: $published_at"
fi

endpoint="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/firecrab-m2publish.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
catalog="${work_dir}/catalog.json"
checksum_file="${work_dir}/SHA256SUMS"

aws_s3() {
  AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" \
  AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
  AWS_DEFAULT_REGION=auto \
    aws --endpoint-url "$endpoint" "$@"
}

# Return success when an object does not exist, but never mistake credentials
# or network failures for an empty registry.
download_existing() {
  local key=$1
  local destination=$2
  local error

  if error=$(aws_s3 s3api head-object --bucket "$R2_BUCKET" --key "$key" 2>&1); then
    aws_s3 s3 cp "s3://${R2_BUCKET}/${key}" "$destination" --only-show-errors
    return
  fi
  case "$error" in
    *'404'*|*'Not Found'*|*'NoSuchKey'*)
      return
      ;;
    *)
      fail "could not inspect s3://${R2_BUCKET}/${key}: $error"
      ;;
  esac
}

info "reading existing registry metadata from ${R2_BUCKET}"
download_existing catalog.json "$catalog"

if [ ! -f "$catalog" ]; then
  printf '{"images":[]}\n' >"$catalog"
fi

jq -e '
  type == "object"
  and (.images | type == "array")
  and ([.images[].alias] | all(type == "string"))
  and ([.images[] | [.alias, (.architecture // "x86_64")]] | length == (unique | length))
  and all(.images[];
    ((.architecture // "x86_64") == "x86_64" or (.architecture // "x86_64") == "aarch64")
    and
    (
      (
        ((.version | type) == "number")
        and (.version >= 1)
        and (.version == (.version | floor))
      )
      or (((.version | type) == "string") and (.version | test("^[1-9][0-9]*$")))
    )
  )
' "$catalog" >/dev/null || fail 'existing catalog.json has an invalid schema'

version=$(jq -er --arg alias "$ALIAS" --arg architecture "$M2IMAGE_ARCH" '
  [.images[]
    | select(.alias == $alias and (.architecture // "x86_64") == $architecture)
    | (.version | tonumber)]
  | if length == 0 then 1 else max + 1 end
' "$catalog")

jq --arg alias "$ALIAS" \
  --arg architecture "$M2IMAGE_ARCH" \
  --arg package "$package_key" \
  --arg sha256 "$archive_sha256" \
  --arg published_at "$published_at" \
  --arg version "$version" \
  --argjson min_disk_gb "$min_disk_gb" '
    .images = (
      [.images[]
        | select(.alias != $alias or (.architecture // "x86_64") != $architecture)] +
      [{
        alias: $alias,
        architecture: $architecture,
        version: $version,
        package: $package,
        sha256: $sha256,
        minDiskGb: $min_disk_gb,
        publishedAt: $published_at
      }]
    )
  ' "$catalog" >"${catalog}.new"
mv -- "${catalog}.new" "$catalog"

printf '%s  %s\n' "$archive_sha256" "$archive_name" >"$checksum_file"

info "uploading ${package_key}"
aws_s3 s3 cp "$archive" "s3://${R2_BUCKET}/${package_key}" \
  --content-type application/zstd --cache-control no-cache --only-show-errors

info "uploading ${checksum_key}"
aws_s3 s3 cp "$checksum_file" "s3://${R2_BUCKET}/${checksum_key}" \
  --content-type text/plain --cache-control no-cache --only-show-errors

# Keep this last: it is the commit point that makes the new package visible.
info 'uploading catalog.json'
aws_s3 s3 cp "$catalog" "s3://${R2_BUCKET}/catalog.json" \
  --content-type application/json --cache-control no-cache --only-show-errors

info "published ${ALIAS}/${M2IMAGE_ARCH} at ${package_key} version ${version} (sha256 ${archive_sha256}, minDiskGb ${min_disk_gb})"
