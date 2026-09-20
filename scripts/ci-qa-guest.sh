#!/usr/bin/env bash
# QA guest boot (public-docs/qa.md I3, V1, V7, V11, V13) on a host with /dev/kvm.
# GitHub Ubuntu: chmod 666 /dev/kvm first (nested virt is not guaranteed).
# Prerequisites: firecrab-api on :5523, outbound registry, KVM rw.
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
TEMPLATE=${1:-alpine-3.24.1}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

pass() { printf 'PASS %s\n' "$1"; }
fail() {
    printf 'FAILED %s: %s\n' "$1" "$2" >&2
    if [ -n "${BODY:-}" ]; then
        printf '%s\n' "$BODY" >&2
    fi
    exit 1
}

CODE=
BODY=

http() {
    local method path body out
    method=$1
    path=$2
    body=${3-}
    out=$(mktemp)
    if [ -n "$body" ]; then
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
            -X "$method" \
            -H "Origin: ${API}" \
            -H 'content-type: application/json' \
            --data "$body" \
            "${API}${path}") || CODE=000
    else
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
            -X "$method" \
            -H "Origin: ${API}" \
            "${API}${path}") || CODE=000
    fi
    BODY=$(cat "$out")
    rm -f "$out"
}

json_get() {
    local expr=$1
    printf '%s' "$BODY" | python3 -c "import json,sys; d=json.load(sys.stdin); print($expr)"
}

poll_job() {
    local id=$1
    local path=$2
    local status
    for _ in $(seq 1 120); do
        http GET "$path"
        if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
            fail "$id" "poll ${path} HTTP ${CODE}"
        fi
        status=$(json_get 'd.get("status") or ""')
        case "$status" in
            succeeded) return 0 ;;
            failed)
                fail "$id" "job failed: $(json_get 'd.get("log") or ""')"
                ;;
            running | idle) sleep 5 ;;
            *)
                fail "$id" "unexpected job status ${status}"
                ;;
        esac
    done
    fail "$id" "timed out waiting for ${path}"
}

test -e /dev/kvm || fail KVM "/dev/kvm is missing"
test -r /dev/kvm && test -w /dev/kvm || fail KVM "runner cannot read/write /dev/kvm"
if id firecrab >/dev/null 2>&1; then
    if ! sudo -u firecrab test -r /dev/kvm; then
        fail KVM "user firecrab cannot read /dev/kvm"
    fi
    if ! sudo -u firecrab test -w /dev/kvm; then
        fail KVM "user firecrab cannot write /dev/kvm"
    fi
fi
pass KVM

http GET "/api/images/${TEMPLATE}"
installed=false
if [ "$CODE" = 200 ]; then
    installed=$(json_get 'str(d.get("installed") or False).lower()')
fi

if [ "$installed" != "true" ]; then
    http POST "/api/images/${TEMPLATE}/package"
    if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
        fail I3 "POST /package HTTP ${CODE}"
    fi
    poll_job I3 "/api/images/${TEMPLATE}/package"
    http POST "/api/images/${TEMPLATE}/install"
    if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
        fail I3 "POST /install HTTP ${CODE}"
    fi
    poll_job I3 "/api/images/${TEMPLATE}/install"
    http GET "/api/images/${TEMPLATE}"
    [ "$CODE" = 200 ] || fail I3 "GET image after install HTTP ${CODE}"
    [ "$(json_get 'str(d.get("installed") or False).lower()')" = "true" ] \
        || fail I3 "template ${TEMPLATE} not installed"
fi
pass I3

"$root/scripts/ci-m2-guest-boot.sh" "$TEMPLATE"
pass V1
pass V7
pass V11
pass V13

http GET /api/vms
[ "$CODE" = 200 ] || fail X1 "GET /api/vms HTTP ${CODE}"
printf '%s' "$BODY" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
bad = [str(r.get("name", "")) for r in rows if str(r.get("name", "")).startswith("qa-") or str(r.get("name", "")).startswith("m2-")]
if bad:
    sys.stderr.write("leftover VMs: " + ", ".join(bad) + "\n")
    sys.exit(1)
' || fail X1 "leftover guest-boot VMs"
pass X1
