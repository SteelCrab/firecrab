#!/usr/bin/env bash
# Unit tests for scripts/firecrab-release.sh (URL, arch, binary pick).
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=firecrab-release.sh
. "$ROOT/scripts/firecrab-release.sh"

failed=0
pass() { printf 'ok  %s\n' "$*"; }
fail() { printf 'not ok  %s\n' "$*" >&2; failed=1; }

expect_eq() {
    local got=$1 want=$2 label=$3
    if [ "$got" = "$want" ]; then
        pass "$label"
    else
        fail "$label (got '$got', want '$want')"
    fi
}

expect_fail() {
    local label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$label (expected failure)"
    else
        pass "$label"
    fi
}

# --- arch -------------------------------------------------------------------

expect_eq "$(firecrab_host_arch x86_64)" x86_64 "arch x86_64"
expect_eq "$(firecrab_host_arch amd64)" x86_64 "arch amd64"
expect_eq "$(firecrab_host_arch aarch64)" aarch64 "arch aarch64"
expect_eq "$(firecrab_host_arch arm64)" aarch64 "arch arm64"
expect_fail "arch ppc64le rejected" firecrab_host_arch ppc64le

expect_eq "$(firecrab_host_tarball x86_64 musl)" firecrab-host-x86_64-musl.tar.gz "tarball x86_64 musl"
expect_eq "$(firecrab_host_tarball x86_64 gnu)" firecrab-host-x86_64-gnu.tar.gz "tarball x86_64 gnu"
expect_eq "$(firecrab_host_tarball aarch64 musl)" firecrab-host-aarch64-musl.tar.gz "tarball aarch64 musl"
expect_eq "$(firecrab_host_tarball aarch64 gnu)" firecrab-host-aarch64-gnu.tar.gz "tarball aarch64 gnu"
expect_eq "$(firecrab_host_tarball x86_64 glibc)" firecrab-host-x86_64-gnu.tar.gz "glibc alias is gnu"
expect_fail "unknown libc rejected" firecrab_host_tarball x86_64 dietlibc

got=$(firecrab_supported_arches | tr '\n' ' ')
expect_eq "${got% }" "x86_64 aarch64" "supported arches are x86_64 and aarch64"
got=$(firecrab_supported_libcs | tr '\n' ' ')
expect_eq "${got% }" "gnu musl" "supported libcs are gnu and musl"
got=$(firecrab_supported_host_tarballs | tr '\n' ' ')
expect_eq "${got% }" "firecrab-host-x86_64-gnu.tar.gz firecrab-host-x86_64-musl.tar.gz firecrab-host-aarch64-gnu.tar.gz firecrab-host-aarch64-musl.tar.gz" "four host bundles"

expect_eq "$(firecrab_normalize_libc gnu)" gnu "normalize gnu"
expect_eq "$(firecrab_normalize_libc glibc)" gnu "normalize glibc"
expect_eq "$(firecrab_normalize_libc musl)" musl "normalize musl"
expect_eq "$(firecrab_host_libc gnu)" gnu "explicit gnu"
expect_eq "$(FIRECRAB_LIBC=musl firecrab_host_libc)" musl "FIRECRAB_LIBC override"
expect_eq "$(FIRECRAB_LIBC=musl firecrab_host_tarball x86_64)" firecrab-host-x86_64-musl.tar.gz "env libc in tarball name"
expect_fail "bad libc rejected" firecrab_host_libc dietlibc

# --- URLs -------------------------------------------------------------------

expect_eq \
    "$(firecrab_release_asset_url latest install.sh)" \
    "https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh" \
    "latest install.sh URL"

expect_eq \
    "$(firecrab_release_asset_url v0.1.0 firecrab-host-x86_64.tar.gz)" \
    "https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/firecrab-host-x86_64.tar.gz" \
    "pinned host tarball URL"

expect_eq \
    "$(firecrab_release_asset_url 0.1.0 SHA256SUMS)" \
    "https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/SHA256SUMS" \
    "bare version gets v prefix"

expect_eq \
    "$(firecrab_release_asset_url "" firecrab-host-x86_64.tar.gz)" \
    "https://github.com/SteelCrab/firecrab/releases/latest/download/firecrab-host-x86_64.tar.gz" \
    "empty version is latest"

expect_eq \
    "$(FIRECRAB_RELEASE_REPO=example/fork firecrab_release_asset_url v0.1.0 install.sh)" \
    "https://github.com/example/fork/releases/download/v0.1.0/install.sh" \
    "FIRECRAB_RELEASE_REPO override"

expect_eq \
    "$(firecrab_install_curl_url latest)" \
    "curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash" \
    "latest curl install line"

expect_eq \
    "$(firecrab_install_curl_url v0.1.0)" \
    "curl -fsSL https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/install.sh | bash" \
    "pinned curl install line"

# --- payload mode -----------------------------------------------------------

expect_eq "$(firecrab_payload_mode "")" release "empty bin-dir is release"
expect_eq "$(firecrab_payload_mode /tmp/bins)" dir "/tmp/bins is dir"

# --- pick a binary from --bin-dir or keep the installed one ----------------

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/src" "$scratch/dst"
printf 'src\n' >"$scratch/src/firecrab-api"
printf 'old\n' >"$scratch/dst/firecrab-net-helper"
chmod +x "$scratch/src/firecrab-api" "$scratch/dst/firecrab-net-helper"

expect_eq \
    "$(firecrab_resolve_binary firecrab-api "$scratch/src" "$scratch/dst")" \
    "$scratch/src/firecrab-api" \
    "prefer --bin-dir when the file exists"

expect_eq \
    "$(firecrab_resolve_binary firecrab-net-helper "$scratch/src" "$scratch/dst")" \
    "$scratch/dst/firecrab-net-helper" \
    "keep installed binary when --bin-dir omits it"

expect_fail "missing both sides is an error" \
    firecrab_resolve_binary firecrab-missing "$scratch/src" "$scratch/dst"

# --- ELF arch of a release binary ------------------------------------------

write_elf64() {
    local dest=$1 machine=$2
    python3 - "$dest" "$machine" <<'PY'
import sys
path, machine = sys.argv[1], int(sys.argv[2])
header = bytearray(64)
header[0:4] = b"\x7fELF"
header[4] = 2  # ELFCLASS64
header[5] = 1  # ELFDATA2LSB
header[6] = 1
header[18:20] = machine.to_bytes(2, "little")
open(path, "wb").write(header)
PY
}

scratch_elf=$(mktemp -d)
write_elf64 "$scratch_elf/x86" 62
write_elf64 "$scratch_elf/arm" 183
printf 'not-elf\n' >"$scratch_elf/junk"

expect_eq "$(firecrab_elf_arch "$scratch_elf/x86")" x86_64 "ELF e_machine 62 is x86_64"
expect_eq "$(firecrab_elf_arch "$scratch_elf/arm")" aarch64 "ELF e_machine 183 is aarch64"
expect_fail "non-ELF rejected" firecrab_elf_arch "$scratch_elf/junk"

if firecrab_assert_binary_arch "$scratch_elf/x86" x86_64; then
    pass "x86_64 binary accepted on x86_64"
else
    fail "x86_64 binary accepted on x86_64"
fi
expect_fail "x86_64 binary rejected on aarch64" \
    firecrab_assert_binary_arch "$scratch_elf/x86" aarch64
expect_fail "aarch64 binary rejected on x86_64" \
    firecrab_assert_binary_arch "$scratch_elf/arm" x86_64
rm -rf "$scratch_elf"

# --- sha256 -----------------------------------------------------------------

printf 'hello\n' >"$scratch/hello.txt"
sum=$(sha256sum "$scratch/hello.txt")
printf '%s\n' "$sum" >"$scratch/SHA256SUMS"
if firecrab_verify_sha256 "$scratch/SHA256SUMS" "$scratch/hello.txt"; then
    pass "sha256 of a matching file"
else
    fail "sha256 of a matching file"
fi
printf 'nope\n' >"$scratch/hello.txt"
if firecrab_verify_sha256 "$scratch/SHA256SUMS" "$scratch/hello.txt" 2>/dev/null; then
    fail "sha256 of a tampered file"
else
    pass "sha256 of a tampered file"
fi

if [ "$failed" -ne 0 ]; then
    printf 'FAILED\n' >&2
    exit 1
fi
printf 'all tests passed\n'
exit 0
