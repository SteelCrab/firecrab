#!/usr/bin/env bash
# macOS E2E: G2 capability gate, fresh install, API QA, and nested guest boot.
# The runner must expose Virtualization.framework nested virtualization.
#
# CI must set isolated FIRECRAB_INSTALL_DIR and FIRECRAB_MICROMANAGER_HOME
# paths. The gate installs the current checkout; the caller purges afterward.
# Usage: scripts/ci-qa-macos-e2e.sh [gate|api|nginx|guest|all]
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
    gate | api | nginx | guest | all) ;;
    *)
        printf 'usage: %s [gate|api|nginx|guest|all]\n' "$0" >&2
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

case "$PHASE" in
    gate)
        run_gate
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
    all)
        run_gate
        "$root/scripts/ci-qa-api.sh"
        "$root/scripts/ci-qa-nginx.sh" nginx:1.27-alpine
        "$root/scripts/ci-qa-guest.sh" alpine:3.21 ubuntu:24.04 fedora:42
        ;;
esac
