#!/usr/bin/env bash
# Nightly / workflow_dispatch helper: create a MicroNetwork, boot one M2
# (guest MicroVM) for the given template, ping it, then stop+delete.
#
# Usage: scripts/ci-m2-guest-boot.sh <template-alias>
#   e.g. scripts/ci-m2-guest-boot.sh alpine-3.24.1
#
# Prerequisites: firecrab-api listening on :3000, a registered template,
# KVM available, micro_network create works (explicit networks only).
set -euo pipefail

TEMPLATE=${1:?template alias required (e.g. alpine-3.24.1)}
API=${FIRECRAB_API:-http://127.0.0.1:3000}
# Unique /24 per template hash so parallel matrix jobs on one host would not
# collide if ever co-located (each CI job is its own runner today).
SUBNET_THIRD=$(printf '%s' "$TEMPLATE" | cksum | awk '{print ($1 % 200) + 20}')
SUBNET="172.${SUBNET_THIRD}.0.0/24"
NAME_NET="boot-$(echo "$TEMPLATE" | tr -c 'a-zA-Z0-9' '-')"
NAME_VM="m2-$(echo "$TEMPLATE" | tr -c 'a-zA-Z0-9' '-')"

echo "M2 boot: template=$TEMPLATE subnet=$SUBNET api=$API"

NET=$(curl -fsS -X POST "$API/api/micro-networks" \
  -H 'content-type: application/json' \
  -d "{\"name\":\"$NAME_NET\",\"subnetCidr\":\"$SUBNET\",\"internetEnabled\":true}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
echo "micro_network_id=$NET"
ip -br link show type bridge | grep -Eq '^mnb' || {
  echo "expected mnb* bridge after MicroNetwork create" >&2
  exit 1
}

VM=$(curl -fsS -X POST "$API/api/vms" \
  -H 'content-type: application/json' \
  -d "{\"name\":\"$NAME_VM\",\"template\":\"$TEMPLATE\",\"cpu\":1,\"ram\":512,\"diskGb\":2,\"microNetworkId\":\"$NET\"}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
echo "vm_id=$VM"

curl -fsS -X POST "$API/api/vms/$VM/start" --max-time 300 >/dev/null

# Poll until running (start may return before guest is ready in some paths).
for _ in $(seq 1 60); do
  state=$(curl -fsS "$API/api/vms/$VM" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')
  echo "state=$state"
  case "$state" in
    running) break ;;
    error|stopped)
      echo "guest entered terminal state $state before running" >&2
      curl -fsS "$API/api/vms/$VM" || true
      exit 1
      ;;
  esac
  sleep 5
done
test "$state" = running

ipv4=$(curl -fsS "$API/api/vms/$VM" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("ipv4") or "")')
test -n "$ipv4"
echo "ipv4=$ipv4"
ping -c 3 -W 5 "$ipv4"

curl -fsS -X POST "$API/api/vms/$VM/stop" --max-time 120 >/dev/null
curl -fsS -X DELETE "$API/api/vms/$VM"

# Network may still be held if delete is slow; best-effort cleanup.
curl -fsS -X DELETE "$API/api/micro-networks/$NET" >/dev/null 2>&1 || true

echo "M2 boot ok: template=$TEMPLATE"
