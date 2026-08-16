#!/usr/bin/env bash
# CLI contract for the binary installer. No root, no host changes.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

failed=0
pass() { printf 'ok  %s\n' "$*"; }
fail() { printf 'not ok  %s\n' "$*" >&2; failed=1; }

help=$("./install.sh" --help)

if printf '%s\n' "$help" | grep -q -- '--bin-dir'; then
    pass "--help mentions --bin-dir"
else
    fail "--help mentions --bin-dir"
fi

if printf '%s\n' "$help" | grep -q -- '--libc'; then
    pass "--help mentions --libc"
else
    fail "--help mentions --libc"
fi

if printf '%s\n' "$help" | grep -q 'firecrab-host-x86_64-gnu.tar.gz' \
    && printf '%s\n' "$help" | grep -q 'firecrab-host-aarch64-musl.tar.gz'; then
    pass "--help lists gnu and musl host bundles"
else
    fail "--help lists gnu and musl host bundles"
fi

if printf '%s\n' "$help" | grep -q -- '--version'; then
    pass "--help mentions --version"
else
    fail "--help mentions --version"
fi

if printf '%s\n' "$help" | grep -q 'releases/latest/download/install.sh'; then
    pass "--help shows the release install URL"
else
    fail "--help shows the release install URL"
fi

if printf '%s\n' "$help" | grep -qi 'cargo build'; then
    fail "--help must not tell operators to cargo build as the default"
else
    pass "--help does not default to cargo build"
fi

got=$("./install.sh" --print-install-url)
want='curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash'
if [ "$got" = "$want" ]; then
    pass "--print-install-url (latest)"
else
    fail "--print-install-url (got '$got')"
fi

got=$("./install.sh" --version v0.1.0 --print-install-url)
want='curl -fsSL https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/install.sh | bash'
if [ "$got" = "$want" ]; then
    pass "--print-install-url (v0.1.0)"
else
    fail "--print-install-url v0.1.0 (got '$got')"
fi

got=$("./install.sh" --version v0.1.0 --libc gnu --print-release-url)
# shellcheck source=firecrab-release.sh
. "$ROOT/scripts/firecrab-release.sh"
want=$(firecrab_release_asset_url v0.1.0 "$(firecrab_host_tarball "" gnu)")
if [ "$got" = "$want" ]; then
    pass "--print-release-url matches helper"
else
    fail "--print-release-url (got '$got', want '$want')"
fi

check=$("./install.sh" --check 2>&1 || true)
if printf '%s\n' "$check" | grep -q 'would download'; then
    pass "--check talks about downloading a release"
else
    fail "--check talks about downloading a release"
fi
if printf '%s\n' "$check" | grep -q 'would install via rustup'; then
    fail "--check must not offer rustup"
else
    pass "--check does not offer rustup"
fi

bindir=$(mktemp -d)
check_dir=$("./install.sh" --bin-dir "$bindir" --check 2>&1 || true)
rmdir "$bindir"
if printf '%s\n' "$check_dir" | grep -q -- "$bindir"; then
    pass "--check --bin-dir mentions the directory"
else
    fail "--check --bin-dir mentions the directory"
fi

# --- published install.sh (helpers inlined, tag baked) ----------------------

pub=$(mktemp -d)
"$ROOT/scripts/bake-install-sh.sh" v0.1.0 "$pub/install.sh"
got=$("$pub/install.sh" --print-install-url)
want='curl -fsSL https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/install.sh | bash'
if [ "$got" = "$want" ]; then
    pass "baked install.sh pins the tag in the curl URL"
else
    fail "baked install.sh pins the tag (got '$got')"
fi

# --- host tarball layout ----------------------------------------------------

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

fake=$(mktemp -d)
write_elf64 "$fake/firecrab-api" 62
write_elf64 "$fake/firecrab-net-helper" 62
mkdir -p "$fake/dist"
printf '<html></html>\n' >"$fake/dist/index.html"
"$ROOT/scripts/package-host-release.sh" x86_64 "$fake" "$fake/dist" "$fake/firecrab-host-x86_64.tar.gz" >/dev/null
members=$(tar -tzf "$fake/firecrab-host-x86_64.tar.gz")
for need in firecrab-api firecrab-net-helper extract-vmlinux extract-arm64-image \
            firecrab-doctor.sh dashboard/index.html systemd/firecrab-api.service \
            systemd/firecrab-net-helper.service; do
    if printf '%s\n' "$members" | grep -qx -- "$need"; then
        pass "host tarball contains $need"
    else
        fail "host tarball contains $need"
    fi
done
wrong=$(mktemp -d)
write_elf64 "$wrong/firecrab-api" 183
write_elf64 "$wrong/firecrab-net-helper" 183
mkdir -p "$wrong/dist"
printf '<html></html>\n' >"$wrong/dist/index.html"
if "$ROOT/scripts/package-host-release.sh" x86_64 "$wrong" "$wrong/dist" "$wrong/out.tar.gz" >/dev/null 2>&1; then
    fail "host tarball rejects aarch64 bins packed as x86_64"
else
    pass "host tarball rejects aarch64 bins packed as x86_64"
fi
rm -rf "$pub" "$fake" "$wrong"

if [ "$failed" -ne 0 ]; then
    printf 'FAILED\n' >&2
    exit 1
fi
printf 'all tests passed\n'
