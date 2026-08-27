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

# Host install prepares the host only; guest images come from OCI import.
piped_check=$(cd /tmp && bash -s -- --check --no-deps <"$pub/install.sh" 2>&1 || true)
if printf '%s\n' "$piped_check" | grep -q 'BASH_SOURCE'; then
    fail "baked --check via stdin must not crash on BASH_SOURCE"
else
    pass "baked --check via stdin does not crash on BASH_SOURCE"
fi

# The advertised one-liner is `curl … | bash`. That leaves BASH_SOURCE unset.
# A production change that re-reads ${BASH_SOURCE[0]} under `set -u` must fail.
piped_out=$(mktemp)
piped_err=$(mktemp)
if (cd /tmp && bash -s -- --print-install-url <"$pub/install.sh" >"$piped_out" 2>"$piped_err"); then
    got=$(cat "$piped_out")
    if [ "$got" = "$want" ]; then
        pass "baked install.sh works when piped to bash (curl | bash)"
    else
        fail "baked install.sh piped (got '$got')"
    fi
else
    fail "baked install.sh piped (exit $?, stderr: $(tr '\n' ' ' <"$piped_err"))"
fi

# `bash <(curl …)` sets BASH_SOURCE to /dev/fd/N — not a checkout.
if got=$(cd /tmp && bash <(cat "$pub/install.sh") --print-install-url); then
    if [ "$got" = "$want" ]; then
        pass "baked install.sh works under process substitution"
    else
        fail "baked install.sh process substitution (got '$got')"
    fi
else
    fail "baked install.sh process substitution (exit $?)"
fi

# The unbaked repo copy still needs scripts/ next to it. The failure must
# be that message, not an unbound BASH_SOURCE crash.
if (cd /tmp && bash -s -- --print-install-url <"$ROOT/install.sh" >"$piped_out" 2>"$piped_err"); then
    fail "repo install.sh piped from /tmp should not succeed"
elif grep -q 'BASH_SOURCE' "$piped_err"; then
    fail "repo install.sh piped from /tmp still crashes on BASH_SOURCE"
elif grep -q 'release helpers missing' "$piped_err"; then
    pass "repo install.sh piped outside a checkout explains missing helpers"
else
    fail "repo install.sh piped from /tmp (stderr: $(tr '\n' ' ' <"$piped_err"))"
fi

# curl | bash has no stdin tty. The installer must prompt on /dev/tty,
# not tell the operator to run `sudo -v` themselves.
if [ "$(id -u)" -ne 0 ] && command -v script >/dev/null 2>&1; then
    fake=$(mktemp -d)
    cat >"$fake/sudo" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >>"${SUDO_LOG:?}"
if [ "$1" = "-n" ]; then
    exit 1
fi
if [ "$1" = "-v" ]; then
    if [ -t 0 ]; then
        printf 'tty\n' >>"$SUDO_LOG"
        exit 0
    fi
    printf 'notty\n' >>"$SUDO_LOG"
    exit 1
fi
exit 0
EOF
    chmod +x "$fake/sudo"
    : >"$fake/log"
    if SUDO_LOG="$fake/log" PATH="$fake:$PATH" \
        script -q -e -c "bash -s -- --uninstall <'$pub/install.sh'" /dev/null \
        >"$piped_out" 2>"$piped_err"; then
        if grep -q 'Run:  sudo -v' "$piped_out" "$piped_err"; then
            fail "piped install.sh must not tell the operator to run sudo -v"
        elif grep -qx tty "$fake/log"; then
            pass "piped install.sh asks sudo on /dev/tty"
        else
            fail "piped install.sh did not prompt sudo on a tty (log: $(tr '\n' ' ' <"$fake/log"))"
        fi
    else
        fail "piped --uninstall via /dev/tty (out: $(tr '\n' ' ' <"$piped_out") err: $(tr '\n' ' ' <"$piped_err"))"
    fi
    rm -rf "$fake"
else
    pass "piped sudo tty prompt skipped (root or no script(1))"
fi
rm -f "$piped_out" "$piped_err"

# FireCrab installs the upstream Firecracker binary from its official release
# archive. That installer must preserve the exact LICENSE/NOTICE/THIRD-PARTY
# files shipped beside the binary and reject an incomplete release archive.
if bash "$ROOT/scripts/test-install-firecracker.sh"; then
    pass "Firecracker release notices are preserved"
else
    fail "Firecracker release notices are preserved"
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
write_elf64 "$fake/firecrab" 62
mkdir -p "$fake/dist"
printf '<html></html>\n' >"$fake/dist/index.html"
"$ROOT/scripts/package-host-release.sh" x86_64 "$fake" "$fake/dist" "$fake/firecrab-host-x86_64.tar.gz" >/dev/null
members=$(tar -tzf "$fake/firecrab-host-x86_64.tar.gz")
for need in firecrab-api firecrab-net-helper extract-vmlinux extract-arm64-image \
            firecrab dashboard/index.html systemd/firecrab-api.service \
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
write_elf64 "$wrong/firecrab" 183
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
