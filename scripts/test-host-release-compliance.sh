#!/usr/bin/env bash
# Host release compliance contract. No root or network access required.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

write_elf64() {
    local dest=$1 machine=$2
    python3 - "$dest" "$machine" <<'PY'
import sys
path, machine = sys.argv[1], int(sys.argv[2])
header = bytearray(64)
header[0:4] = b"\x7fELF"
header[4] = 2
header[5] = 1
header[6] = 1
header[18:20] = machine.to_bytes(2, "little")
open(path, "wb").write(header)
PY
    chmod +x "$dest"
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bins="$work/bins"
dashboard="$work/dashboard"
compliance="$work/compliance"
unpacked="$work/unpacked"
mkdir -p "$bins" "$dashboard" "$compliance" "$unpacked"

write_elf64 "$bins/firecrab-api" 62
write_elf64 "$bins/firecrab-net-helper" 62
write_elf64 "$bins/firecrab" 62
printf '<html></html>\n' >"$dashboard/index.html"
printf 'third-party notices\n' >"$compliance/THIRD_PARTY_NOTICES.txt"
printf '{"schemaVersion":1,"runtime":[],"buildTestOnly":[]}\n' \
    >"$compliance/release-license-inventory.json"

bundle="$work/firecrab-host-x86_64-gnu.tar.gz"
"$ROOT/scripts/package-host-release.sh" \
    x86_64 "$bins" "$dashboard" "$bundle" "$compliance" >/dev/null

members=$(tar -tzf "$bundle")
for need in \
    LICENSE \
    THIRD_PARTY_NOTICES.txt \
    release-license-inventory.json \
    licenses/GPL-2.0-only.txt \
    extract-vmlinux; do
    printf '%s\n' "$members" | grep -qx -- "$need" || {
        printf 'missing compliance member: %s\n' "$need" >&2
        exit 1
    }
done

tar -xzf "$bundle" -C "$unpacked"
cmp "$ROOT/LICENSE" "$unpacked/LICENSE"
cmp "$ROOT/licenses/GPL-2.0-only.txt" "$unpacked/licenses/GPL-2.0-only.txt"
cmp "$compliance/THIRD_PARTY_NOTICES.txt" "$unpacked/THIRD_PARTY_NOTICES.txt"
cmp "$compliance/release-license-inventory.json" "$unpacked/release-license-inventory.json"

rm "$compliance/THIRD_PARTY_NOTICES.txt"
if "$ROOT/scripts/package-host-release.sh" \
    x86_64 "$bins" "$dashboard" "$work/missing.tar.gz" "$compliance" \
    >/dev/null 2>"$work/error"; then
    echo 'packager accepted a missing THIRD_PARTY_NOTICES.txt' >&2
    exit 1
fi
grep -q 'missing .*THIRD_PARTY_NOTICES.txt' "$work/error"

printf 'host release compliance tests passed\n'
