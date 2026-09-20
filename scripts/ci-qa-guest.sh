#!/usr/bin/env bash
# QA guest boot from OCI images (public-docs/qa.md I5, I6, V1, V7, V11, V13, X5).
# Default references: alpine, ubuntu, fedora from Docker Hub.
# GitHub Ubuntu: chmod 666 /dev/kvm first (nested virt is not guaranteed).
# Prerequisites: firecrab-api on :5523, fakeroot, outbound registry, KVM rw.
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [ $# -eq 0 ]; then
    set -- alpine:3.21 ubuntu:24.04 fedora:42
fi
REFERENCES=("$@")

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

inspect_ref() {
    local ref=$1
    local out
    out=$(mktemp)
    CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
        -G "${API}/api/oci/inspect" \
        --data-urlencode "reference=${ref}" \
        -H "Origin: ${API}") || CODE=000
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
    for _ in $(seq 1 240); do
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

if [ "$(uname -s)" = Linux ]; then
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
else
    pass KVM
fi

boot_oci() {
    local reference=$1
    local alias installed disk body
    printf 'OCI guest boot reference=%s\n' "$reference"

    inspect_ref "$reference"
    [ "$CODE" = 200 ] || fail "I5/${reference}" "inspect HTTP ${CODE}"
    alias=$(json_get 'd["alias"]')
    [ -n "$alias" ] || fail "I5/${reference}" "inspect missing alias"
    pass "I5/${reference} alias=${alias}"

    http GET "/api/images/${alias}"
    installed=false
    if [ "$CODE" = 200 ]; then
        installed=$(json_get 'str(d.get("installed") or False).lower()')
    fi

    if [ "$installed" != "true" ]; then
        body=$(python3 -c 'import json,sys; print(json.dumps({"reference":sys.argv[1]}))' "$reference")
        http POST /api/oci/import "$body"
        if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
            fail "I6/${reference}" "POST /api/oci/import HTTP ${CODE}"
        fi
        poll_job "I6/${reference}" "/api/oci/import/${alias}"
        http GET "/api/images/${alias}"
        [ "$CODE" = 200 ] || fail "I6/${reference}" "GET image after import HTTP ${CODE}"
        [ "$(json_get 'str(d.get("installed") or False).lower()')" = "true" ] \
            || fail "I6/${reference}" "alias ${alias} not installed"
    fi
    pass "I6/${alias}"

    disk=$(json_get 'd.get("minDiskGb") or 2')
    FIRECRAB_QA_DISK_GB=$disk "$root/scripts/ci-m2-guest-boot.sh" "$alias"
    pass "V1/${alias}"
    pass "V7/${alias}"
    pass "V11/${alias}"
    pass "V13/${alias}"

    http DELETE "/api/images/${alias}"
    if [ "$CODE" != 204 ] && [ "$CODE" != 200 ]; then
        fail "X5/${alias}" "DELETE image HTTP ${CODE}"
    fi
    pass "X5/${alias}"
}

for REFERENCE in "${REFERENCES[@]}"; do
    boot_oci "$REFERENCE"
done

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
