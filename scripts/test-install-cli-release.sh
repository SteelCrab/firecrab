#!/usr/bin/env bash
# Cross-platform CLI installer contract and a local end-to-end install.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
failed=0
pass() { printf 'ok  %s\n' "$*"; }
fail() { printf 'not ok  %s\n' "$*" >&2; failed=1; }

expect_eq() {
    local got=$1 want=$2 label=$3
    if [ "$got" = "$want" ]; then pass "$label"; else fail "$label (got '$got', want '$want')"; fi
}

expect_eq \
    "$(FIRECRAB_CLI_OS=Linux FIRECRAB_CLI_ARCH=x86_64 "$ROOT/install-cli.sh" --print-asset)" \
    firecrab-cli-x86_64-linux.tar.gz \
    "Linux x86_64 asset"
expect_eq \
    "$(FIRECRAB_CLI_OS=Darwin FIRECRAB_CLI_ARCH=arm64 "$ROOT/install-cli.sh" --print-asset)" \
    firecrab-cli-aarch64-macos.tar.gz \
    "Apple silicon asset"
expect_eq \
    "$(FIRECRAB_CLI_OS=linux FIRECRAB_CLI_ARCH=aarch64 "$ROOT/install-cli.sh" --version 1.2.3 --print-url)" \
    "https://github.com/SteelCrab/firecrab/releases/download/v1.2.3/firecrab-cli-aarch64-linux.tar.gz" \
    "bare version becomes a v-prefixed release URL"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
release="$scratch/releases/latest/download"
mkdir -p "$release/payload" "$scratch/bin"
printf '#!/bin/sh\nprintf "firecrab test\\n"\n' >"$release/payload/firecrab"
chmod +x "$release/payload/firecrab"
tar -czf "$release/firecrab-cli-x86_64-linux.tar.gz" -C "$release/payload" firecrab
(
    cd "$release"
    sha256sum firecrab-cli-x86_64-linux.tar.gz >SHA256SUMS
)

FIRECRAB_RELEASE_BASE="file://$scratch/releases" \
FIRECRAB_CLI_OS=Linux \
FIRECRAB_CLI_ARCH=x86_64 \
PATH="/usr/bin:/bin" \
    "$ROOT/install-cli.sh" --install-dir "$scratch/bin" >/dev/null
if [ -x "$scratch/bin/firecrab" ] && [ "$("$scratch/bin/firecrab")" = "firecrab test" ]; then
    pass "local release installs an executable client"
else
    fail "local release installs an executable client"
fi

victim="$scratch/victim"
printf 'leave this file alone\n' >"$victim"
unlink "$scratch/bin/firecrab"
ln -s "$victim" "$scratch/bin/firecrab"
FIRECRAB_RELEASE_BASE="file://$scratch/releases" \
FIRECRAB_CLI_OS=Linux \
FIRECRAB_CLI_ARCH=x86_64 \
    "$ROOT/install-cli.sh" --install-dir "$scratch/bin" >/dev/null
if [ ! -L "$scratch/bin/firecrab" ] \
    && [ "$(cat "$victim")" = "leave this file alone" ] \
    && [ "$("$scratch/bin/firecrab")" = "firecrab test" ]; then
    pass "existing destination symlink is replaced without following it"
else
    fail "existing destination symlink is replaced without following it"
fi

if env -u HOME \
    FIRECRAB_CLI_OS=Linux FIRECRAB_CLI_ARCH=x86_64 \
    "$ROOT/install-cli.sh" --install-dir "$scratch/bin" --check >/dev/null; then
    pass "explicit install directory does not require HOME"
else
    fail "explicit install directory does not require HOME"
fi

relative_output=$(
    cd "$scratch"
    FIRECRAB_RELEASE_BASE="file://$scratch/releases" \
    FIRECRAB_CLI_OS=Linux FIRECRAB_CLI_ARCH=x86_64 \
        "$ROOT/install-cli.sh" --install-dir relative-bin
)
absolute_bin="$(cd "$scratch" && pwd -P)/relative-bin"
if [[ "$relative_output" == *"export PATH=\"$absolute_bin:"* ]] \
    && [ "$(cd / && PATH="$absolute_bin:/usr/bin:/bin" firecrab)" = "firecrab test" ]; then
    pass "relative install directory produces a usable absolute PATH"
else
    fail "relative install directory produces a usable absolute PATH"
fi

printf 'tampered\n' >>"$release/firecrab-cli-x86_64-linux.tar.gz"
if FIRECRAB_RELEASE_BASE="file://$scratch/releases" \
    FIRECRAB_CLI_OS=Linux FIRECRAB_CLI_ARCH=x86_64 \
    "$ROOT/install-cli.sh" --install-dir "$scratch/bin" >/dev/null 2>&1; then
    fail "tampered archive is rejected"
else
    pass "tampered archive is rejected"
fi

if [ "$failed" -ne 0 ]; then
    printf 'FAILED\n' >&2
    exit 1
fi
printf 'all tests passed\n'
