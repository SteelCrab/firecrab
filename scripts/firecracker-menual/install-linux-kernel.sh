#!/usr/bin/env bash
# Manual entry point for the same native builder embedded in firecrab-api.
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "${script_dir}/../.." && pwd -P)
build_dir="${repo_dir}/build/kernel-local"
artifact_dir="${repo_dir}/images/kernel"
releases_url=${FIRECRAB_KERNEL_RELEASES_URL:-https://www.kernel.org/releases.json}
[ "$(uname -s)" = Linux ] || { printf 'Kernel builds require a Linux host\n' >&2; exit 1; }
case "$(uname -m)" in
  x86_64) architecture=x86_64 ;;
  aarch64|arm64) architecture=aarch64 ;;
  *) printf 'Unsupported host architecture\n' >&2; exit 1 ;;
esac
for tool in curl python3 bash; do
  command -v "$tool" >/dev/null || { printf 'Required command not found: %s\n' "$tool" >&2; exit 1; }
done
case "$releases_url" in none|-) printf 'Kernel source downloads are disabled\n' >&2; exit 1 ;; esac
mkdir -p "$build_dir/downloads" "$artifact_dir"
attempt=$(mktemp -d "$build_dir/attempt.XXXXXXXX")
trap 'rm -rf -- "$attempt"' EXIT
curl -fsSL --connect-timeout 15 --max-time 60 "$releases_url" -o "$attempt/releases.json"
read -r version source_url < <(python3 - "$attempt/releases.json" <<'PY'
import json, re, sys
with open(sys.argv[1], encoding="utf-8") as source:
    releases = json.load(source)["releases"]
release = next((r for r in releases if r.get("moniker") == "stable" and not r.get("iseol", False)), None)
if not release or not re.fullmatch(r"[1-9][0-9]*\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?", release["version"]):
    raise SystemExit("Could not resolve kernel.org stable release")
version = release["version"]
url = f"https://cdn.kernel.org/pub/linux/kernel/v{version.split('.')[0]}.x/linux-{version}.tar.xz"
if release.get("source") != url:
    raise SystemExit("Unexpected kernel.org stable source URL")
print(version, url)
PY
)
curl -fsSL --connect-timeout 15 --max-time 60 "${source_url%/*}/sha256sums.asc" -o "$attempt/sha256sums.asc"
checksum=$(python3 - "$attempt/sha256sums.asc" "$version" <<'PY'
import re, sys
with open(sys.argv[1], encoding="utf-8") as sums:
    for line in sums:
        fields = line.split()
        if len(fields) == 2 and fields[1].lstrip('*') == f"linux-{sys.argv[2]}.tar.xz" and re.fullmatch(r"[a-fA-F0-9]{64}", fields[0]):
            print(fields[0].lower())
            break
    else:
        raise SystemExit("Kernel source checksum not found")
PY
)
archive="$build_dir/downloads/linux-$version.tar.xz"
verify_source() {
  python3 - "$1" "$checksum" <<'PY'
import hashlib, sys
try:
    with open(sys.argv[1], 'rb') as source:
        digest = hashlib.sha256()
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
except OSError:
    raise SystemExit(1)
raise SystemExit(0 if digest.hexdigest() == sys.argv[2] else 1)
PY
}
if ! verify_source "$archive"; then
  curl -fL --retry 2 --connect-timeout 15 "$source_url" -o "$attempt/source.tar.xz"
  verify_source "$attempt/source.tar.xz" || { printf 'Kernel source SHA256 mismatch\n' >&2; exit 1; }
  mv "$attempt/source.tar.xz" "$archive"
fi
# Keep the historical filename consumed by the manual serial-console scripts.
image="$artifact_dir/vmlinux-$version-$architecture"
printf '[INFO] Building kernel.org stable %s for %s\n' "$version" "$architecture"
bash "$repo_dir/scripts/build-host-kernel.sh" "$architecture" "$version" "$archive" "$attempt/build" "$attempt/kernel"
mv "$attempt/kernel" "$image"
mv "$attempt/kernel.config" "$image.config"
printf '[INFO] Kernel image: %s\n' "$image"
