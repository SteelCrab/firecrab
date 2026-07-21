#!/usr/bin/env bash
# Create or delete a batch of test VMs against a running firecrab-api, for
# manually exercising concurrent start/disk-prep behavior
# (docs/bugs/vm-startup-stuck-under-concurrent-load.md).

set -u

API="${FIRECRAB_API:-http://127.0.0.1:3000}"
ORIGIN="${FIRECRAB_ORIGIN:-http://localhost:8080}"
STATE_FILE="${FIRECRAB_BATCH_STATE:-/tmp/firecrab-vm-batch.ids}"

usage() {
  cat <<EOF
Usage:
  $(basename "$0") create [count] [prefix]   # default count=10, prefix=batch
  $(basename "$0") start                     # POST /start for every VM in $STATE_FILE
  $(basename "$0") stop                      # POST /stop for every VM in $STATE_FILE
  $(basename "$0") delete                    # DELETE every VM in $STATE_FILE, then clear it
  $(basename "$0") status                    # current state of every VM in $STATE_FILE

Env:
  FIRECRAB_API=$API
  FIRECRAB_ORIGIN=$ORIGIN
  FIRECRAB_BATCH_STATE=$STATE_FILE   (tracks ids created by this script across calls)
EOF
}

require_jq() {
  command -v jq >/dev/null 2>&1 || {
    echo "jq is required" >&2
    exit 1
  }
}

cmd_create() {
  count="${1:-10}"
  prefix="${2:-batch}"
  : >"$STATE_FILE"
  echo "creating $count VM(s) with prefix '$prefix'..."
  for i in $(seq 1 "$count"); do
    id=$(curl -s -X POST "$API/api/vms" \
      -H "Content-Type: application/json" -H "Origin: $ORIGIN" \
      -d "{\"name\":\"${prefix}${i}\",\"template\":\"ubuntu-26.04\",\"cpu\":1,\"ram\":512,\"diskGb\":2}" \
      | jq -r '.id // empty')
    if [ -z "$id" ]; then
      echo "  [$i] create failed" >&2
      continue
    fi
    echo "$id" >>"$STATE_FILE"
    echo "  [$i] $id"
  done
  echo "$(wc -l <"$STATE_FILE" | tr -d ' ') VM id(s) saved to $STATE_FILE"
}

each_id() {
  [ -s "$STATE_FILE" ] || {
    echo "no ids in $STATE_FILE — run 'create' first" >&2
    exit 1
  }
  while IFS= read -r id; do
    [ -n "$id" ] && "$@" "$id" &
  done <"$STATE_FILE"
  wait
}

cmd_start() {
  each_id start_one
}
start_one() {
  report_async_result "$1" "$(curl -s -w '\n%{http_code}' -X POST "$API/api/vms/$1/start" -H "Origin: $ORIGIN")"
}

cmd_stop() {
  each_id stop_one
}
stop_one() {
  report_async_result "$1" "$(curl -s -w '\n%{http_code}' -X POST "$API/api/vms/$1/stop" -H "Origin: $ORIGIN")"
}

# A 504 here just means the response took longer than the server's request
# timeout — start_vm's actual work is detached (tokio::spawn) and keeps
# running regardless (docs/bugs/vm-startup-stuck-under-concurrent-load.md),
# so this isn't a failure, just "check status later" instead of "now".
report_async_result() {
  id="$1"
  http_code=$(printf '%s' "$2" | tail -n1)
  body=$(printf '%s' "$2" | sed '$d')
  if [ "$http_code" = "504" ]; then
    echo "  ${id:0:8} -> timed out client-side, still running server-side (check: status)"
  else
    state=$(printf '%s' "$body" | jq -r '.state // .error.message // "unknown"')
    echo "  ${id:0:8} -> $state"
  fi
}

cmd_delete() {
  each_id delete_one
  : >"$STATE_FILE"
}
delete_one() {
  status=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$API/api/vms/$1" -H "Origin: $ORIGIN")
  echo "  ${1:0:8} -> HTTP $status"
}

cmd_status() {
  [ -s "$STATE_FILE" ] || {
    echo "no ids in $STATE_FILE — run 'create' first" >&2
    exit 1
  }
  curl -s "$API/api/vms" -H "Origin: $ORIGIN" \
    | jq -r --slurpfile ids <(jq -R . <"$STATE_FILE" | jq -s .) \
      '.[] | select(.id as $i | $ids[0] | index($i)) | "\(.name)\t\(.state)\t\(.startupStep // "-")\t\(.id)"' \
    | column -t
}

require_jq
case "${1:-}" in
  create) shift; cmd_create "$@" ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  delete) cmd_delete ;;
  status) cmd_status ;;
  *) usage; exit 1 ;;
esac
