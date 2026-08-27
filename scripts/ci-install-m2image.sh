#!/usr/bin/env bash
# CI helper: make one built-in M2Image available through the same
# MicroRegistry package -> verified cache -> install path used by the UI.
set -euo pipefail

ALIAS=${1:?template alias required (e.g. alpine-3.24.1)}
API=${FIRECRAB_API:-http://127.0.0.1:5523}
POLL_ATTEMPTS=${FIRECRAB_IMAGE_POLL_ATTEMPTS:-300}
POLL_SECONDS=${FIRECRAB_IMAGE_POLL_SECONDS:-2}

image_installed() {
  curl -fsS "$API/api/images" | python3 -c 'import json,sys; alias=sys.argv[1]; images=json.load(sys.stdin); print("true" if any(i.get("alias") == alias and i.get("installed") for i in images) else "false")' "$ALIAS"
}

snapshot_field() {
  local endpoint=$1 field=$2
  curl -fsS "$endpoint" | python3 -c "import json,sys; print(json.load(sys.stdin).get('$field') or '')"
}

wait_for_job() {
  local endpoint=$1 label=$2 status log
  for _ in $(seq 1 "$POLL_ATTEMPTS"); do
    status=$(snapshot_field "$endpoint" status)
    case "$status" in
      succeeded)
        return 0
        ;;
      failed)
        log=$(snapshot_field "$endpoint" log)
        printf '%s failed for %s:\n%s\n' "$label" "$ALIAS" "$log" >&2
        return 1
        ;;
      idle)
        # A 409 can race with an already-running operation. Give the API a
        # moment to publish that job before treating idle as terminal.
        ;;
      running) ;;
      *) echo "unexpected $label status for $ALIAS: $status" >&2 ;;
    esac
    sleep "$POLL_SECONDS"
  done
  log=$(snapshot_field "$endpoint" log || true)
  printf '%s timed out for %s\n%s\n' "$label" "$ALIAS" "$log" >&2
  return 1
}

installed=$(image_installed)
if [ "$installed" = true ]; then
  echo "M2Image already installed: $ALIAS"
  exit 0
fi

echo "M2Image package: $ALIAS"
body=$(mktemp)
trap 'rm -f "$body"' EXIT
code=$(curl -sS -o "$body" -w '%{http_code}' -X POST "$API/api/images/$ALIAS/package")
case "$code" in
  202) ;;
  409) echo "package request already active: $(cat "$body")" ;;
  *) echo "package request for $ALIAS: HTTP $code $(cat "$body")" >&2; exit 1 ;;
esac
wait_for_job "$API/api/images/$ALIAS/package" package

installed=$(image_installed)
if [ "$installed" = true ]; then
  echo "M2Image became available while package was prepared: $ALIAS"
  exit 0
fi

echo "M2Image install: $ALIAS"
code=$(curl -sS -o "$body" -w '%{http_code}' -X POST "$API/api/images/$ALIAS/install")
case "$code" in
  202) ;;
  409)
    # A long-lived self-hosted runner may have registered the image between
    # our checks. Accept only a real installed template, not any 409.
    installed=$(image_installed)
    if [ "$installed" = true ]; then
      echo "M2Image already installed: $ALIAS"
      exit 0
    fi
    echo "image install conflict for $ALIAS: $(cat "$body")" >&2
    exit 1
    ;;
  *) echo "image install for $ALIAS: HTTP $code $(cat "$body")" >&2; exit 1 ;;
esac
wait_for_job "$API/api/images/$ALIAS/install" install

installed=$(image_installed)
test "$installed" = true || {
  echo "install reported success but $ALIAS is not registered" >&2
  exit 1
}
echo "M2Image ready: $ALIAS"
