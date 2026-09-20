#!/usr/bin/env bash
# Self-hosted macOS E2E: G2 doctor, then API + nested Firecracker guest boot.
# GitHub-hosted ARM64 macOS runners do not support nested virtualization.
# This script is for a real Mac runner (`runs-on: [self-hosted, macOS]`).
#
# Does not `firecrab service install`. The runner must already have a
# provisioned management VM; this job starts it if needed.
set -euo pipefail

if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'ci-qa-macos-e2e.sh runs on macOS only' >&2
    exit 2
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
export FIRECRAB_API=$API

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
    printf '%s\n' 'FAILED G2: firecrab service doctor --json is not ready' >&2
    firecrab service doctor || true
    exit 1
}
printf 'PASS G2\n'

wait_api() {
    for _ in $(seq 1 60); do
        if curl -fsS --connect-timeout 2 --max-time 5 "${API}/api/host" >/dev/null; then
            return 0
        fi
        sleep 3
    done
    return 1
}

if ! curl -fsS --connect-timeout 2 --max-time 5 "${API}/api/host" >/dev/null; then
    printf 'API down; firecrab service start\n'
    firecrab service start || true
    wait_api || {
        printf '%s\n' 'FAILED G2: API not reachable after service start. Run firecrab service install on this runner once.' >&2
        firecrab service status || true
        exit 1
    }
fi
printf 'PASS G2 API\n'

"$root/scripts/ci-qa-api.sh"
"$root/scripts/ci-qa-guest.sh" alpine:3.21 ubuntu:24.04 fedora:42
