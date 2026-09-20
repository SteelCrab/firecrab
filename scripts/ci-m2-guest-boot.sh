#!/usr/bin/env bash
# Nightly / workflow_dispatch helper: create a MicroNetwork, boot one M2
# (guest MicroVM) for the given template, ping it, then stop+delete.
#
# When FIRECRAB_QA_FIRST_REFERENCE=1, the VM lifecycle also drives the CLI
# (create/list/start/console/stop/delete) and exercises the API-only storage
# assignment and busy-network guards. Later references intentionally retain
# the API flow below so one OCI run does not multiply those control-plane rows.
#
# Usage: scripts/ci-m2-guest-boot.sh <template-alias>
#   e.g. scripts/ci-m2-guest-boot.sh alpine-3.24.1
#
# Prerequisites: firecrab-api listening on :5523, a registered template,
# KVM available, micro_network create works (explicit networks only).
set -euo pipefail

TEMPLATE=${1:?template alias required (e.g. alpine-3.24.1)}
API=${FIRECRAB_API:-http://127.0.0.1:5523}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# Unique /24 per template hash so parallel matrix jobs on one host would not
# collide if ever co-located (each CI job is its own runner today).
SUBNET_THIRD=$(printf '%s' "$TEMPLATE" | cksum | awk '{print ($1 % 200) + 20}')
SUBNET="172.${SUBNET_THIRD}.0.0/24"
NAME_NET="boot-$(echo "$TEMPLATE" | tr -c 'a-zA-Z0-9' '-')"
NAME_VM="m2-$(echo "$TEMPLATE" | tr -c 'a-zA-Z0-9' '-')"
FIRST_REFERENCE=${FIRECRAB_QA_FIRST_REFERENCE:-0}
CLI_BIN=${FIRECRAB_QA_CLI:-}

if [ "$FIRST_REFERENCE" = 1 ]; then
  if [ -z "$CLI_BIN" ]; then
    CLI_BIN=$(command -v firecrab || true)
  fi
  [ -n "$CLI_BIN" ] || {
    echo "firecrab CLI is required for the first OCI reference" >&2
    exit 1
  }
fi

cli() {
  "$CLI_BIN" --api "$API" "$@"
}

echo "M2 boot: template=$TEMPLATE subnet=$SUBNET api=$API"

NET=
VM=
DELETED_VM=
STORAGE=
STORAGE_PATH=
CONSOLE_ERROR=
cleanup_boot() {
  # This trap runs for both the happy path and a failed lifecycle assertion.
  # Keep every cleanup operation best-effort: the original failure is more
  # useful than a second error from a partially torn-down VM/network.
  if [ -n "${VM:-}" ]; then
    curl -sS -o /dev/null --max-time 120 -X POST "$API/api/vms/$VM/stop" || true
    curl -sS -o /dev/null --max-time 30 -X DELETE "$API/api/vms/$VM" || true
  fi
  if [ -n "${STORAGE:-}" ]; then
    curl -sS -o /dev/null --max-time 30 -X DELETE "$API/api/micro-storages/$STORAGE" || true
  fi
  if [ -n "${NET:-}" ]; then
    curl -sS -o /dev/null --max-time 15 -X DELETE "$API/api/micro-networks/$NET" || true
  fi
  if [ -n "${STORAGE_PATH:-}" ] && [ "$(uname -s)" = Linux ]; then
    rmdir "$STORAGE_PATH" 2>/dev/null || true
  fi
  if [ -n "${CONSOLE_ERROR:-}" ]; then
    rm -f "$CONSOLE_ERROR"
  fi
}
trap cleanup_boot EXIT

NET=$(curl -fsS -X POST "$API/api/micro-networks" \
  -H 'content-type: application/json' \
  -d "{\"name\":\"$NAME_NET\",\"subnetCidr\":\"$SUBNET\",\"internetEnabled\":true}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
echo "micro_network_id=$NET"
if [ "$(uname -s)" = Linux ]; then
  ip -br link show type bridge | grep -Eq '^mnb' || {
    echo "expected mnb* bridge after MicroNetwork create" >&2
    exit 1
  }
fi

if [ "$FIRST_REFERENCE" = 1 ]; then
  echo "creating first VM through firecrab CLI"
  cli vm list --json >/dev/null || {
    echo "firecrab vm list failed before create" >&2
    exit 1
  }
  VM=$(cli vm create \
    --name "$NAME_VM" \
    --template "$TEMPLATE" \
    --network "$NET" \
    --cpu 1 \
    --ram "${FIRECRAB_QA_RAM:-1024}" \
    --disk-gb "${FIRECRAB_QA_DISK_GB:-2}" \
    --json \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  echo "vm_id=$VM"

  cli_vms=$(cli vm list --json)
  printf '%s' "$cli_vms" | python3 -c '
import json, sys
if not any(row.get("id") == sys.argv[1] for row in json.load(sys.stdin)):
    raise SystemExit("CLI VM list did not show the created VM")
' "$VM" || {
    echo "firecrab vm list did not show the created VM" >&2
    exit 1
  }
else
  VM=$(curl -fsS -X POST "$API/api/vms" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"$NAME_VM\",\"template\":\"$TEMPLATE\",\"cpu\":1,\"ram\":${FIRECRAB_QA_RAM:-1024},\"diskGb\":${FIRECRAB_QA_DISK_GB:-2},\"microNetworkId\":\"$NET\"}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  echo "vm_id=$VM"
fi

if [ "$FIRST_REFERENCE" = 1 ]; then
  # V2 is intentionally API-backed, not inferred from CLI output.
  vm_list=$(curl -fsS --max-time 30 "$API/api/vms")
  printf '%s' "$vm_list" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
want = sys.argv[1]
matches = [row for row in rows if row.get("id") == want]
if len(matches) != 1 or matches[0].get("state") != "created":
    raise SystemExit("API VM list did not show exactly one created VM")
' "$VM" || {
    echo "V2 API VM list did not show the CLI-created VM" >&2
    exit 1
  }
  vm_detail=$(curl -fsS --max-time 30 "$API/api/vms/$VM")
  printf '%s' "$vm_detail" | python3 -c '
import json, sys
d = json.load(sys.stdin)
if d.get("id") != sys.argv[1] or d.get("state") != "created":
    raise SystemExit("API VM detail is not the created CLI VM")
' "$VM" || {
    echo "V2 API VM detail did not show the CLI-created VM" >&2
    exit 1
  }
  echo "PASS V2/$TEMPLATE (API list/detail)"

  # Register a disposable storage root from the API. The path is resolved by
  # the API host, which keeps this valid through the macOS localhost tunnel.
  # Keep it short because Firecracker's nested runtime socket also lives under
  # this root and Unix-domain socket paths have a small platform limit.
  storage_suffix=$(printf '%s' "$TEMPLATE" | tr -c 'a-zA-Z0-9' '-')-$$
  STORAGE_PATH=${FIRECRAB_QA_STORAGE_PATH:-/tmp/fcqa-${VM%%-*}}
  storage_json=$(curl -fsS --max-time 30 -X POST "$API/api/micro-storages" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"qa-m2-storage-$storage_suffix\",\"path\":\"$STORAGE_PATH\"}")
  STORAGE=$(printf '%s' "$storage_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  [ -n "$STORAGE" ] || {
    echo "V6 MicroStorage response did not contain an id" >&2
    exit 1
  }
  storage_vm=$(curl -fsS --max-time 30 -X PUT "$API/api/vms/$VM/storage" \
    -H 'content-type: application/json' \
    -d "{\"storageRoot\":\"$STORAGE\"}")
  printf '%s' "$storage_vm" | python3 -c '
import json, sys
d = json.load(sys.stdin)
if d.get("id") != sys.argv[1] or d.get("storageRoot") != sys.argv[2]:
    raise SystemExit("storage assignment response did not contain the requested root")
' "$VM" "$STORAGE" || {
    echo "V6 storageRoot was not updated" >&2
    exit 1
  }
  echo "PASS V6/$TEMPLATE storageRoot=$STORAGE"

  # The VM has a lease as soon as it is created, so the network must reject a
  # delete even before the guest is started.
  network_delete_code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 30 \
    -X DELETE "$API/api/micro-networks/$NET")
  [ "$network_delete_code" = 409 ] || {
    echo "N6 expected network DELETE 409, got $network_delete_code" >&2
    exit 1
  }
  echo "PASS N6/$TEMPLATE (network DELETE while VM attached -> 409)"

  cli vm list --json >/dev/null || {
    echo "firecrab vm list failed after create" >&2
    exit 1
  }
  cli vm start "$VM" --json >/dev/null || {
    echo "firecrab vm start failed" >&2
    exit 1
  }
else
  curl -fsS -X POST "$API/api/vms/$VM/start" --max-time 300 >/dev/null
fi

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
if [ "$(uname -s)" = Linux ]; then
  ping -c 3 -W 5 "$ipv4"
else
  # Nested Firecracker lives inside the management guest. The Mac/Windows
  # runner cannot ping 172.31.x.x; the serial marker is the reachability check.
  curl -fsS "$API/api/vms/$VM/log" | python3 -c '
import json, sys
log = json.load(sys.stdin).get("consoleLog") or ""
if "FIRECRAB_NETWORK_READY" not in log:
    sys.stderr.write("missing FIRECRAB_NETWORK_READY in console log\n")
    sys.exit(1)
'
fi

if [ "$FIRST_REFERENCE" = 1 ]; then
  # stdin is a pipe, so the CLI does not enter raw terminal mode. Ctrl+] is
  # the documented detach byte and proves the WebSocket upgrade completed.
  CONSOLE_ERROR=$(mktemp)
  if ! printf '\035' | cli vm console "$VM" >/dev/null 2>"$CONSOLE_ERROR"; then
    cat "$CONSOLE_ERROR" >&2
    rm -f "$CONSOLE_ERROR"
    CONSOLE_ERROR=
    echo "V9/C2 CLI console attach failed" >&2
    exit 1
  fi
  rm -f "$CONSOLE_ERROR"
  CONSOLE_ERROR=
  echo "PASS V9/$TEMPLATE (console WebSocket)"
  echo "PASS C2/$TEMPLATE (CLI console attach/detach)"

  running_delete_code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 30 \
    -X DELETE "$API/api/vms/$VM")
  [ "$running_delete_code" = 409 ] || {
    echo "V12 expected VM DELETE 409 while running, got $running_delete_code" >&2
    exit 1
  }
  echo "PASS V12/$TEMPLATE (running VM DELETE -> 409)"
fi

FIRECRAB_API=$API "$root/scripts/ci-qa-ssh.sh" "$VM" "$ipv4"

if [ "$FIRST_REFERENCE" = 1 ]; then
  cli vm stop "$VM" --json >/dev/null || {
    echo "firecrab vm stop failed" >&2
    exit 1
  }
  for _ in $(seq 1 30); do
    state=$(curl -fsS "$API/api/vms/$VM" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])')
    [ "$state" = stopped ] && break
    sleep 2
  done
  [ "$state" = stopped ] || {
    echo "VM did not reach stopped after firecrab vm stop (state=$state)" >&2
    exit 1
  }
  cli vm delete "$VM" || {
    echo "firecrab vm delete failed" >&2
    exit 1
  }
else
  curl -fsS -X POST "$API/api/vms/$VM/stop" --max-time 120 >/dev/null
  curl -fsS -X DELETE "$API/api/vms/$VM"
fi
DELETED_VM=$VM
VM=
if [ "$FIRST_REFERENCE" = 1 ]; then
  if [ "$(curl -sS -o /dev/null -w '%{http_code}' --max-time 30 "$API/api/vms/$DELETED_VM")" != 404 ]; then
    echo "V13 expected GET deleted VM 404" >&2
    exit 1
  fi
  remaining_vms=$(curl -fsS --max-time 30 "$API/api/vms")
  printf '%s' "$remaining_vms" | python3 -c '
import json, sys
if any(row.get("id") == sys.argv[1] for row in json.load(sys.stdin)):
    raise SystemExit("deleted VM remains in API list")
' "$DELETED_VM" || {
    echo "V13 deleted VM remains in API list" >&2
    exit 1
  }
  cli_vms=$(cli vm list --json)
  printf '%s' "$cli_vms" | python3 -c '
import json, sys
if any(row.get("id") == sys.argv[1] for row in json.load(sys.stdin)):
    raise SystemExit("CLI VM list still contains the deleted VM")
' "$DELETED_VM" || {
    echo "firecrab vm list failed after delete" >&2
    exit 1
  }
  echo "PASS C1/$TEMPLATE (CLI list/create/start/stop/delete)"
  echo "PASS V13/$TEMPLATE (CLI delete + API 404/list absence)"
fi
if [ -n "${STORAGE:-}" ]; then
  curl -fsS -X DELETE "$API/api/micro-storages/$STORAGE" >/dev/null
  STORAGE=
fi
curl -fsS -X DELETE "$API/api/micro-networks/$NET" >/dev/null 2>&1 || true
NET=

echo "M2 boot ok: template=$TEMPLATE"
