#!/usr/bin/env bash
# macOS E2E: G2 capability gate, fresh install, G6 service shell, API QA, and
# nested guest boot.
# The runner must expose Virtualization.framework nested virtualization.
#
# CI must set isolated FIRECRAB_INSTALL_DIR and FIRECRAB_MICROMANAGER_HOME
# paths. The gate installs the current checkout; the caller purges afterward.
# Usage: scripts/ci-qa-macos-e2e.sh [gate|shell|repair|browser|api|nginx|guest|lifetime|all]
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'ci-qa-macos-e2e.sh runs on macOS only' >&2
    exit 2
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
export FIRECRAB_API=$API
PHASE=${1:-all}
# Guests boot three virtualization layers deep (VZ -> KVM -> Firecracker), where
# fedora's first-boot sshd outlasts the default QA waits.
export FIRECRAB_QA_WAIT_FACTOR=${FIRECRAB_QA_WAIT_FACTOR:-3}

case "$PHASE" in
    gate | shell | repair | browser | api | nginx | guest | lifetime | all) ;;
    *)
        printf 'usage: %s [gate|shell|repair|browser|api|nginx|guest|lifetime|all]\n' "$0" >&2
        exit 2
        ;;
esac

if [ -z "${FIRECRAB_MICROMANAGER_HELPER:-}" ]; then
    for candidate in \
        "$root/target/debug/firecrab-micromanager-macos" \
        "$root/target/release/firecrab-micromanager-macos"; do
        if [ -x "$candidate" ]; then
            export FIRECRAB_MICROMANAGER_HELPER=$candidate
            break
        fi
    done
fi

if [ -x "$root/target/debug/firecrab" ]; then
    PATH="$root/target/debug:$PATH"
elif [ -x "$root/target/release/firecrab" ]; then
    PATH="$root/target/release:$PATH"
fi
export PATH

command -v firecrab >/dev/null 2>&1 || {
    printf '%s\n' 'FAILED G2: firecrab CLI not on PATH' >&2
    exit 1
}

require_api() {
    curl -fsS --connect-timeout 2 --max-time 5 "${API}/api/host" >/dev/null || {
        printf '%s\n' 'FAILED G2: management API is not reachable; run the gate phase first' >&2
        exit 1
    }
}

configure_manager_ssh() {
    local managed_home marker manager_key manager_host
    managed_home=${FIRECRAB_MICROMANAGER_HOME:?FIRECRAB_MICROMANAGER_HOME must be an isolated CI path}
    marker="$managed_home/runtime/manager-ready"
    manager_key="$managed_home/runtime/manager_ed25519"

    [ -r "$marker" ] || {
        printf 'FAILED G2: missing management VM marker %s\n' "$marker" >&2
        exit 1
    }
    [ -r "$manager_key" ] || {
        printf 'FAILED G2: missing management VM SSH key %s\n' "$manager_key" >&2
        exit 1
    }
    manager_host=$(sed -n 's/^ip=//p' "$marker")
    [ -n "$manager_host" ] || {
        printf 'FAILED G2: management VM marker has no ip: %s\n' "$marker" >&2
        exit 1
    }

    export FIRECRAB_QA_MANAGER_HOST=$manager_host
    export FIRECRAB_QA_MANAGER_KEY=$manager_key
    export FIRECRAB_QA_REQUIRE_LIVE_GUEST=1
}

shell_failed() {
    printf 'FAILED G6: %s\n' "$1" >&2
    exit 1
}

# G6: `service shell` runs a command in the managed Debian guest as root,
# passes each argument through unchanged, returns the command's exit code, and
# asks for a terminal only when stdin is one; without a command it is a login.
run_shell() {
    local output code=0
    output=$(firecrab service shell -- id -un </dev/null) || shell_failed "service shell exited with $?"
    [ "$output" = root ] || shell_failed "commands run as '$output', expected root"
    output=$(firecrab service shell -- systemctl is-active firecrab-api </dev/null) || true
    [ "$output" = active ] || shell_failed "firecrab-api is '$output' in the shell's guest, expected active"
    # shellcheck disable=SC2016 # `$HOME` must reach the guest unexpanded.
    set -- '[%s]\n' "it's here" 'a b' '$HOME'
    output=$(firecrab service shell -- printf "$@" </dev/null) || shell_failed "printf exited with $?"
    # shellcheck disable=SC2059 # the format is the first test argument.
    [ "$output" = "$(printf "$@")" ] || shell_failed "arguments arrived as: $output"
    firecrab service shell -- sh -c 'exit 7' </dev/null || code=$?
    [ "$code" = 7 ] || shell_failed "a command that exits 7 returned $code"
    output=$(firecrab service shell -- tty </dev/null) || true
    [ "$output" = 'not a tty' ] || shell_failed "a command without a terminal got one: $output"
    # `script` gives the CLI a terminal as stdin.
    script -q /dev/null firecrab service shell -- tty </dev/null | grep -q '/dev/pts/' ||
        shell_failed "a command run from a terminal got no terminal"
    printf 'id -un\nexit\n' | script -q /dev/null firecrab service shell | tr -d '\r' | grep -qx root ||
        shell_failed "the interactive shell is not a root login"
    printf 'PASS G6 service shell runs as root, keeps arguments and exit codes, and opens a login\n'
}

run_gate() {
    : "${FIRECRAB_INSTALL_DIR:?FIRECRAB_INSTALL_DIR must be an isolated CI path}"
    : "${FIRECRAB_MICROMANAGER_HOME:?FIRECRAB_MICROMANAGER_HOME must be an isolated CI path}"
    printf 'G2 doctor\n'
    firecrab service doctor --json | python3 -c '
import json, sys
d = json.load(sys.stdin)
if d.get("ready") is True:
    sys.exit(0)
checks = d.get("checks") or []
print(json.dumps(d, indent=2))
sys.exit(1)
' || {
        printf '%s\n' 'FAILED G2: nested virtualization capability is not ready' >&2
        firecrab service doctor || true
        exit 1
    }
    printf 'G2 install current checkout\n'
    firecrab service install --yes
    firecrab service status
    require_api
    configure_manager_ssh
    printf 'PASS G2 capability, fresh install, service status, and management SSH\n'
}

# R1–R7: VMs in systemd units across stops, restarts, and crashes.
run_lifetime() {
    "$root/scripts/ci-qa-lifetime.sh" "${FIRECRAB_QA_LIFETIME_REFERENCE:-alpine:3.21}"
}

run_repair() {
    python3 "$root/scripts/ci-qa-macos-repair.py" --cli "$(command -v firecrab)"
}

case "$PHASE" in
    gate)
        run_gate
        ;;
    shell)
        require_api
        run_shell
        ;;
    repair)
        run_repair
        ;;
    browser)
        require_api
        configure_manager_ssh
        npm --prefix "$root/firecrab-e2e" test
        ;;
    api)
        require_api
        "$root/scripts/ci-qa-api.sh"
        ;;
    nginx)
        require_api
        configure_manager_ssh
        "$root/scripts/ci-qa-nginx.sh" nginx:1.27-alpine
        ;;
    guest)
        require_api
        configure_manager_ssh
        "$root/scripts/ci-qa-guest.sh" alpine:3.21 ubuntu:24.04 fedora:42
        ;;
    lifetime)
        require_api
        configure_manager_ssh
        run_lifetime
        ;;
    all)
        run_gate
        run_shell
        run_repair
        configure_manager_ssh
        npm --prefix "$root/firecrab-e2e" test
        "$root/scripts/ci-qa-api.sh"
        "$root/scripts/ci-qa-nginx.sh" nginx:1.27-alpine
        "$root/scripts/ci-qa-guest.sh" alpine:3.21 ubuntu:24.04 fedora:42
        run_lifetime
        ;;
esac
