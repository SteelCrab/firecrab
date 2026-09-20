#!/usr/bin/env bash
# QA guest boot (public-docs/qa.md I3, V1, V7, V11, V13) on a host with /dev/kvm.
# Default templates are the three catalog M2Images (alpine, ubuntu, rocky).
# There is no fedora catalog image; rocky-9.8 is the RHEL-family third.
# GitHub Ubuntu: chmod 666 /dev/kvm first (nested virt is not guaranteed).
# Prerequisites: firecrab-api on :5523, outbound registry, KVM rw.
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [ $# -eq 0 ]; then
    set -- alpine-3.24.1 ubuntu-26.04 rocky-9.8
fi
TEMPLATES=("$@")

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

boot_template() {
    local template=$1
    local installed
    printf 'guest boot template=%s\n' "$template"
    http GET "/api/images/${template}"
    installed=false
    if [ "$CODE" = 200 ]; then
        installed=$(json_get 'str(d.get("installed") or False).lower()')
    fi

    if [ "$installed" != "true" ]; then
        http POST "/api/images/${template}/package"
        if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
            fail "I3/${template}" "POST /package HTTP ${CODE}"
        fi
        poll_job "I3/${template}" "/api/images/${template}/package"
        http POST "/api/images/${template}/install"
        if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
            fail "I3/${template}" "POST /install HTTP ${CODE}"
        fi
        poll_job "I3/${template}" "/api/images/${template}/install"
        http GET "/api/images/${template}"
        [ "$CODE" = 200 ] || fail "I3/${template}" "GET image after install HTTP ${CODE}"
        [ "$(json_get 'str(d.get("installed") or False).lower()')" = "true" ] \
            || fail "I3/${template}" "template ${template} not installed"
    fi
    pass "I3/${template}"

    "$root/scripts/ci-m2-guest-boot.sh" "$template"
    pass "V1/${template}"
    pass "V7/${template}"
    pass "V11/${template}"
    pass "V13/${template}"
}

for TEMPLATE in "${TEMPLATES[@]}"; do
    boot_template "$TEMPLATE"
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
