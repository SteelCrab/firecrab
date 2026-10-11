#!/usr/bin/env bash
# Real-guest regression: writes without a user sync survive CLI and dashboard
# API stop/start. Run on the Linux host with an installed SSH-capable template.
# Usage: FIRECRAB_CLI=target/release/firecrab bash scripts/ci-qa-stop-persistence.sh ubuntu-resolute
# FIRECRAB_QA_VM_ID can select an already prepared disposable VM named qa-*.
# That VM is left running; only resources created by this script are deleted.
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
CLI=${FIRECRAB_CLI:-firecrab}
TEMPLATE=${1:-ubuntu-26.04}
SUBNET=${FIRECRAB_QA_SUBNET:-172.30.97.0/24}
WORK=$(mktemp -d)
VM=${FIRECRAB_QA_VM_ID:-}
OWN_VM=0
NET=

http() {
    local method=$1 path=$2
    shift 2
    curl -fsS --max-time 180 -X "$method" -H "Origin: $API" \
        -H 'content-type: application/json' "$API$path" "$@"
}
field() { python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }
cleanup() {
    if [ "$OWN_VM" = 1 ]; then
        http POST "/api/vms/$VM/stop" >/dev/null 2>&1 || true
        http DELETE "/api/vms/$VM" >/dev/null 2>&1 || true
    fi
    if [ -n "$NET" ]; then http DELETE "/api/micro-networks/$NET" >/dev/null 2>&1 || true; fi
    rm -rf "$WORK"
}
trap cleanup EXIT

if [ -n "$VM" ]; then
    NAME=$(http GET "/api/vms/$VM" | field name)
    case "$NAME" in qa-*) ;; *) printf 'Prepared VM must be named qa-*\n' >&2; exit 1 ;; esac
else
    NET=$(http POST /api/micro-networks --data \
    "{\"name\":\"qa-stop-$$\",\"subnetCidr\":\"$SUBNET\",\"internetEnabled\":true}" | field id)
    VM=$(http POST /api/vms --data \
    "{\"name\":\"qa-stop-$$\",\"template\":\"$TEMPLATE\",\"cpu\":1,\"ram\":512,\"diskGb\":4,\"microNetworkId\":\"$NET\"}" | field id)
    OWN_VM=1
fi
printf 'Temporary VM: %s (%s)\n' "$VM" "$TEMPLATE"

start() {
    if [ "$1" = cli ]; then
        "$CLI" --api "$API" vm start "$VM" --json >/dev/null
    else
        http POST "/api/vms/$VM/start" >/dev/null
    fi
    local state
    for _ in $(seq 1 180); do
        state=$(http GET "/api/vms/$VM" | field state)
        if [ "$state" = running ]; then return; fi
        if [ "$state" != starting ]; then printf 'Unexpected state: %s\n' "$state" >&2; exit 1; fi
        sleep 1
    done
    printf 'Guest did not start\n' >&2
    exit 1
}
if [ "$(http GET "/api/vms/$VM" | field state)" != running ]; then start cli; fi
IP=$(http GET "/api/vms/$VM" | field ipv4)
http GET "/api/vms/$VM/ssh-key" > "$WORK/key"
chmod 600 "$WORK/key"
PUBLIC=$(http GET "/api/vms/$VM/ssh-host-key" | field publicKey)
printf '%s %s\n' "$IP" "$PUBLIC" > "$WORK/known_hosts"
guest() {
    ssh -F /dev/null -T -i "$WORK/key" -o IdentitiesOnly=yes -o BatchMode=yes \
        -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$WORK/known_hosts" \
        -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=3 "root@$IP" "$@"
}
ready() {
    for _ in $(seq 1 "${FIRECRAB_QA_SSH_ATTEMPTS:-180}"); do
        if guest true >/dev/null 2>&1; then return; fi
        sleep 1
    done
    printf 'Guest SSH did not become ready\n' >&2
    exit 1
}

for cycle in 1 2 3; do
    for mode in cli web-api; do
        ready
        token="persist-$cycle-$mode-$VM"
        # Intentionally no sync/fsync before the stop request.
        guest "printf '%s\\n' '$token' > /root/qa-$cycle-$mode.txt"
        if [ "$mode" = cli ]; then
            "$CLI" --api "$API" vm stop "$VM" --json >/dev/null
        else
            http POST "/api/vms/$VM/stop" >/dev/null
        fi
        start "$mode"
        ready
        actual=$(guest "cat /root/qa-$cycle-$mode.txt")
        [ "$actual" = "$token" ] || { printf 'Lost write: %s\n' "$token" >&2; exit 1; }
        printf 'PASS cycle=%s mode=%s bytes=%s\n' "$cycle" "$mode" "${#actual}"
    done
done
# Detect a later cycle corrupting an earlier write, including zero-byte files.
for cycle in 1 2 3; do
    for mode in cli web-api; do
        [ "$(guest "cat /root/qa-$cycle-$mode.txt")" = "persist-$cycle-$mode-$VM" ]
    done
done
printf 'PASS all six files survived all subsequent stop/start cycles\n'
