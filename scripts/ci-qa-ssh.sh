#!/usr/bin/env bash
# QA V8: operator key, host-key fingerprint, live check=match, ssh login.
# Usage: scripts/ci-qa-ssh.sh <vm-id> [ipv4]
# Optional FIRECRAB_QA_SSH_KEY: keep the PEM at that path (caller deletes it).
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
VM_ID=${1:?vm id required}
IPV4=${2-}

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
OWN_KEY=0
KEY=${FIRECRAB_QA_SSH_KEY-}

cleanup() {
    if [ "$OWN_KEY" = 1 ] && [ -n "${KEY:-}" ]; then
        rm -f "$KEY"
    fi
}
trap cleanup EXIT

http() {
    local method path out
    method=$1
    path=$2
    out=$(mktemp)
    CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 45 \
        -X "$method" \
        -H "Origin: ${API}" \
        "${API}${path}") || CODE=000
    BODY=$(cat "$out")
    rm -f "$out"
}

json_get() {
    local expr=$1
    printf '%s' "$BODY" | python3 -c "import json,sys; d=json.load(sys.stdin); print($expr)"
}

if [ -z "$KEY" ]; then
    KEY=$(mktemp)
    chmod 600 "$KEY"
    OWN_KEY=1
fi

curl -sS --connect-timeout 5 --max-time 30 -o "$KEY" "${API}/api/vms/${VM_ID}/ssh-key" \
    || fail V8a "GET /ssh-key failed"
grep -q 'BEGIN OPENSSH PRIVATE KEY\|BEGIN.*PRIVATE KEY' "$KEY" \
    || fail V8a "ssh-key is not a PEM"
pass V8a

http GET "/api/vms/${VM_ID}/ssh-host-key"
[ "$CODE" = 200 ] || fail V8b "GET /ssh-host-key HTTP ${CODE}"
fp=$(json_get 'd.get("fingerprint") or ""')
[ -n "$fp" ] || fail V8b "missing fingerprint"
pass "V8b ${fp}"

check_ok=0
status=
for _ in $(seq 1 40); do
    http GET "/api/vms/${VM_ID}/ssh-host-key/check"
    if [ "$CODE" = 200 ]; then
        status=$(json_get 'd.get("status") or ""')
        printf 'ssh-host-key/check status=%s\n' "$status"
        if [ "$status" = match ]; then
            check_ok=1
            break
        fi
    fi
    sleep 3
done
[ "$check_ok" = 1 ] || fail V8c "GET /ssh-host-key/check expected match got ${status:-HTTP ${CODE}}"
pass V8c

if [ -z "$IPV4" ]; then
    http GET "/api/vms/${VM_ID}"
    [ "$CODE" = 200 ] || fail V8d "GET vm HTTP ${CODE}"
    IPV4=$(json_get 'd.get("ipv4") or ""')
fi
[ -n "$IPV4" ] || fail V8d "no ipv4 for ssh"

if [ "$(uname -s)" = Linux ]; then
    ssh_ok=0
    for _ in $(seq 1 30); do
        if ssh -i "$KEY" \
            -o StrictHostKeyChecking=no \
            -o UserKnownHostsFile=/dev/null \
            -o IdentitiesOnly=yes \
            -o ConnectTimeout=5 \
            -o BatchMode=yes \
            "root@${IPV4}" true >/dev/null 2>&1; then
            ssh_ok=1
            break
        fi
        sleep 3
    done
    [ "$ssh_ok" = 1 ] || fail V8d "ssh root@${IPV4} never became ready"
    uname_out=$(ssh -i "$KEY" \
        -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null \
        -o IdentitiesOnly=yes \
        -o ConnectTimeout=5 \
        -o BatchMode=yes \
        "root@${IPV4}" uname -s)
    [ -n "$uname_out" ] || fail V8d "ssh uname empty"
    pass "V8d ssh root@${IPV4} ${uname_out}"
else
    pass "V8d skip live ssh on $(uname -s)"
fi
