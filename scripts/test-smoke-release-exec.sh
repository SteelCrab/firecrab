#!/usr/bin/env bash
# Unit tests for scripts/smoke-release-exec.sh.
# firecrab-api is a long-lived server: a process that is still running when
# the budget expires must pass. 126/127 (cannot exec) must fail.
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT=$ROOT/scripts/smoke-release-exec.sh

failed=0
pass() { printf 'ok  %s\n' "$*"; }
fail() { printf 'not ok  %s\n' "$*" >&2; failed=1; }

expect_ok() {
    local label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label (expected success)"
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

[ -x "$SCRIPT" ] || {
    printf 'not ok  smoke-release-exec.sh missing or not executable\n' >&2
    exit 1
}

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

printf '#!/bin/sh\nexit 1\n' >"$scratch/exits-one"
chmod +x "$scratch/exits-one"
printf '#!/bin/sh\nsleep 30\n' >"$scratch/hangs"
chmod +x "$scratch/hangs"
printf 'not a program\n' >"$scratch/not-exec"

# The hang that stalled v0.1.0: a server that never exits must not block
# the caller past --seconds, and must be treated as an executable success.
start=$(date +%s)
expect_ok "still-running process is success" \
    "$SCRIPT" --seconds 1 -- "$scratch/hangs"
elapsed=$(( $(date +%s) - start ))
if [ "$elapsed" -lt 8 ]; then
    pass "hang returns before 8s (took ${elapsed}s)"
else
    fail "hang took ${elapsed}s; timeout did not fire"
fi

expect_ok "immediate non-zero exit is success (binary ran)" \
    "$SCRIPT" --seconds 2 -- "$scratch/exits-one"

expect_fail "missing command is failure" \
    "$SCRIPT" --seconds 2 -- "$scratch/does-not-exist"

expect_fail "non-executable file is failure" \
    "$SCRIPT" --seconds 2 -- "$scratch/not-exec"

expect_fail "no command is usage error" \
    "$SCRIPT" --seconds 1

if [ "$failed" -ne 0 ]; then
    exit 1
fi
printf 'all smoke-release-exec tests passed\n'
