#!/usr/bin/env bash
# Shared QA work list from public-docs/qa.md against a live API (:5523).
# IDs match the cross-platform QA sheet (G/H/N/S/L/I/V/C/X).
#
# RUN:  G4 G5 H1 H2 N1-N5 S1-S3 L1-L3 I1 I8(GET) I9 V14
#       C3 C5 when `firecrab` is on PATH; X1-X4 X6
# SKIP: G1  Linux install job (doctor / install / status)
#       G2  GitHub macOS hosted: build only; E2E is self-hosted Mac
#       G3  Windows microManager job; E2E is a manual Windows host
#       U1  do not POST /api/update
#       N6  DELETE busy needs a VM
#       I2 I3 I4 I7 I10  M2Image/bootstrap not in this script
#       I5 I6 + V* boot: ci-qa-guest.sh (OCI alpine/ubuntu/fedora)
#       V1-V13     ci-qa-guest.sh on Ubuntu with /dev/kvm
#       C1 C2 C4   guest boot / image import
#       X5         no custom OCI alias
# N4: IPv6 SLAAC POST; HTTP 400 → SKIP with body, do not fail.
# Prefix qa-ci-; IPv4 172.31.201.0/24. Never name a network "ci".
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
PREFIX=qa-ci-
SUBNET=172.31.201.0/24
SUBNET_V6=172.31.211.0/24
SUBNET_CLI=172.31.203.0/24
STORAGE_PATH=${FIRECRAB_QA_STORAGE_PATH:-/tmp/qa-ci-pool}

NET_ID=
NET_V6_ID=
NET_CLI_ID=
STORAGE_ID=
SHELL_ID=
VM_ID=
CODE=
BODY=
FIRECRAB=

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

pass() { printf 'PASS %s\n' "$1"; }
skip() { printf 'SKIP %s: %s\n' "$1" "$2"; }

fail() {
    printf 'FAILED %s: %s\n' "$1" "$2" >&2
    if [ -n "${BODY:-}" ]; then
        printf '%s\n' "$BODY" >&2
    fi
    exit 1
}

cleanup() {
    if [ -n "${SHELL_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/shells/${SHELL_ID}" || true
    fi
    if [ -n "${STORAGE_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/micro-storages/${STORAGE_ID}" || true
    fi
    if [ -n "${VM_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/vms/${VM_ID}" || true
    fi
    if [ -n "${NET_CLI_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/micro-networks/${NET_CLI_ID}" || true
    fi
    if [ -n "${NET_V6_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/micro-networks/${NET_V6_ID}" || true
    fi
    if [ -n "${NET_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/micro-networks/${NET_ID}" || true
    fi
}
trap cleanup EXIT

http() {
    local method path body out
    method=$1
    path=$2
    body=${3-}
    out=$(mktemp)
    if [ -n "$body" ]; then
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 45 \
            -X "$method" \
            -H "Origin: ${API}" \
            -H 'content-type: application/json' \
            --data "$body" \
            "${API}${path}") || CODE=000
    else
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 45 \
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

expect() {
    local id=$1
    local want=$2
    if [ "$CODE" != "$want" ]; then
        fail "$id" "expected HTTP $want got ${CODE:-empty}"
    fi
}

require_request_id() {
    local id=$1
    local rid
    rid=$(json_get 'd.get("error", {}).get("requestId") or ""')
    if [ -n "$rid" ]; then
        return 0
    fi
    fail "$id" "JSON 404 missing error.requestId"
}

leftover_qa() {
    local path=$1
    http GET "$path"
    expect "cleanup ${path}" 200
    printf '%s' "$BODY" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
if not isinstance(rows, list):
    sys.stderr.write("not a JSON list\n")
    sys.exit(1)
bad = [str(r.get("name", "")) for r in rows if str(r.get("name", "")).startswith("qa-")]
if bad:
    sys.stderr.write("leftover: " + ", ".join(bad) + "\n")
    sys.exit(1)
'
}

command -v curl >/dev/null 2>&1 || fail setup "curl is required"
command -v python3 >/dev/null 2>&1 || fail setup "python3 is required"

if command -v firecrab >/dev/null 2>&1; then
    FIRECRAB=$(command -v firecrab)
elif [ -x /usr/local/bin/firecrab ]; then
    FIRECRAB=/usr/local/bin/firecrab
elif [ -x "$root/target/debug/firecrab" ]; then
    FIRECRAB=$root/target/debug/firecrab
elif [ -x "$root/target/release/firecrab" ]; then
    FIRECRAB=$root/target/release/firecrab
fi

skip G1 "Linux install job covers doctor/install/status"
skip G2 "macOS microManager job covers service doctor"
skip G3 "Windows microManager job covers service doctor"
skip U1 "do not POST /api/update"
skip N6 "DELETE busy needs a VM"
skip I2 "kernel install skipped in this script"
skip I3 "M2Image package/install not in OCI guest boot"
skip I4 "kernel pair skipped in this script"
skip I5 "OCI inspect runs in ci-qa-guest.sh"
skip I6 "OCI import runs in ci-qa-guest.sh"
skip I7 "microregistry register skipped in this script"
skip I10 "bootstrap skipped in this script"
skip V1 "guest boot runs in ci-qa-guest.sh"
skip V2 "guest boot runs in ci-qa-guest.sh"
skip V3 "guest boot runs in ci-qa-guest.sh"
skip V4 "guest boot runs in ci-qa-guest.sh"
skip V5 "guest boot runs in ci-qa-guest.sh"
skip V6 "guest boot runs in ci-qa-guest.sh"
skip V7 "guest boot runs in ci-qa-guest.sh"
skip V8 "guest boot runs in ci-qa-guest.sh"
skip V9 "guest boot runs in ci-qa-guest.sh"
skip V10 "guest boot runs in ci-qa-guest.sh"
skip V11 "guest boot runs in ci-qa-guest.sh"
skip V12 "guest boot runs in ci-qa-guest.sh"
skip V13 "guest boot runs in ci-qa-guest.sh"
skip C1 "guest boot runs in ci-qa-guest.sh"
skip C2 "guest boot runs in ci-qa-guest.sh"
skip C4 "guest boot image CLI runs in ci-qa-guest.sh"
skip X5 "no custom OCI alias"

http GET /api/host
expect G4 200
json_get 'd' >/dev/null
http GET /
expect G4 200
printf '%s' "$BODY" | python3 -c '
import sys
body = sys.stdin.read().lower()
if "html" not in body:
    sys.exit(1)
' || fail G4 "dashboard GET / was not HTML"
pass G4

http GET /api/no-such-route
expect G5 404
require_request_id G5
pass G5

http GET /api/update
expect H1 200
json_get 'd.get("current")' >/dev/null
pass H1

http GET /api/network
expect H2 200
printf '%s' "$BODY" | python3 -c '
import json, sys
d = json.load(sys.stdin)
uplink = d.get("uplink") or ""
if not uplink:
    sys.stderr.write("missing uplink\n")
    sys.exit(1)
for name in d.get("interfaces") or []:
    if name == "lo" or str(name).startswith("fct") or str(name).startswith("mnb"):
        sys.stderr.write("picker has " + str(name) + "\n")
        sys.exit(1)
' || fail H2 "uplink/interfaces check failed"
pass H2

http POST /api/micro-networks \
    "{\"name\":\"${PREFIX}net\",\"subnetCidr\":\"${SUBNET}\",\"internetEnabled\":true}"
expect N1 201
NET_ID=$(json_get 'd["id"]')
[ -n "$NET_ID" ] || fail N1 "create response missing id"
pass N1

http GET /api/micro-networks
expect N2 200
printf '%s' "$BODY" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
want_id, want_name, want_cidr = sys.argv[1], sys.argv[2], sys.argv[3]
found = [r for r in rows if r.get("id") == want_id]
if not found:
    sys.exit(1)
if found[0].get("name") != want_name or found[0].get("subnetCidr") != want_cidr:
    sys.exit(1)
' "$NET_ID" "${PREFIX}net" "$SUBNET" || fail N2 "list missing created network"
http GET "/api/micro-networks/${NET_ID}"
expect N2 200
[ "$(json_get 'd["id"]')" = "$NET_ID" ] || fail N2 "detail id mismatch"
pass N2

http PATCH "/api/micro-networks/${NET_ID}" '{"internetEnabled":false}'
expect N3 200
[ "$(json_get 'json.dumps(d["internetEnabled"])')" = "false" ] || fail N3 "expected internetEnabled false"
http PATCH "/api/micro-networks/${NET_ID}" '{"internetEnabled":true}'
expect N3 200
[ "$(json_get 'json.dumps(d["internetEnabled"])')" = "true" ] || fail N3 "expected internetEnabled true"
pass N3

http POST /api/micro-networks \
    "{\"name\":\"${PREFIX}v6\",\"subnetCidr\":\"${SUBNET_V6}\",\"internetEnabled\":true,\"ipv6AddressMode\":\"slaac\"}"
if [ "$CODE" = 400 ]; then
    skip N4 "HTTP 400 ${BODY}"
elif [ "$CODE" = 201 ]; then
    NET_V6_ID=$(json_get 'd["id"]')
    [ -n "$NET_V6_ID" ] || fail N4 "create response missing id"
    pass N4
else
    fail N4 "expected HTTP 201 or 400 got ${CODE:-empty}"
fi

http POST /api/micro-networks \
    "{\"name\":\"${PREFIX}bad-uplink\",\"subnetCidr\":\"172.31.204.0/24\",\"uplink\":\"\"}"
expect N5 400
pass N5

http GET /api/storage
expect S1 200
printf '%s' "$BODY" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
if not any(r.get("id") == "default" for r in rows):
    sys.exit(1)
' || fail S1 "GET /api/storage missing default root"
pass S1

http GET /api/storage/devices
expect S2 200
json_get 'd' >/dev/null
pass S2

http POST /api/micro-storages \
    "{\"name\":\"${PREFIX}pool\",\"path\":\"${STORAGE_PATH}\"}"
expect S3 201
STORAGE_ID=$(json_get 'd["id"]')
[ -n "$STORAGE_ID" ] || fail S3 "create response missing id"
pass S3

SHELL_BODY=$(python3 -c 'import json; print(json.dumps({"name":"qa-ci-sh","content":"#!/bin/sh\necho qa-ci-sh\n"}))')
http POST /api/shells "$SHELL_BODY"
expect L1 201
SHELL_ID=$(json_get 'd["shellId"]')
[ -n "$SHELL_ID" ] || fail L1 "create response missing shellId"
pass L1

REV_BODY=$(python3 -c 'import json; print(json.dumps({"content":"#!/bin/sh\necho qa-ci-sh-2\n"}))')
http POST "/api/shells/${SHELL_ID}/revisions" "$REV_BODY"
expect L2 201
REV_ID=$(json_get 'd["revisionId"]')
[ -n "$REV_ID" ] || fail L2 "revision response missing revisionId"
pass L2

http GET "/api/shells/${SHELL_ID}"
expect L3 200
http GET "/api/shells/${SHELL_ID}/revisions/${REV_ID}"
expect L3 200
[ "$(json_get 'd["revisionId"]')" = "$REV_ID" ] || fail L3 "revision GET mismatch"
pass L3

http GET /api/images
expect I1 200
http GET /api/kernels
expect I1 200
http GET /api/microregistry
if [ "$CODE" != 200 ] && [ "$CODE" != 503 ]; then
    fail I1 "GET /api/microregistry expected 200 or 503 got ${CODE:-empty}"
fi
pass I1

http GET /api/microregistry/docker-hub
expect I8 200
json_get 'd["configured"]' >/dev/null
pass I8

http GET /api/images/does-not-exist
expect I9 404
require_request_id I9
pass I9

http POST /api/vms '{"name":"qa-ci-nonet","template":"alpine-3.24","cpu":1,"ram":512,"diskGb":2}'
if [ "$CODE" = 201 ]; then
    VM_ID=$(json_get 'd.get("id") or ""')
    fail V14 "create without microNetworkId returned 201"
fi
expect V14 400
pass V14

if [ -n "$FIRECRAB" ]; then
    FIRECRAB_API=$API "$FIRECRAB" network list --json >/dev/null \
        || fail C3 "firecrab network list failed"
    CLI_JSON=$(FIRECRAB_API=$API "$FIRECRAB" network create \
        --name "${PREFIX}cli" --subnet-cidr "$SUBNET_CLI" --json) \
        || fail C3 "firecrab network create failed"
    NET_CLI_ID=$(printf '%s' "$CLI_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
    [ -n "$NET_CLI_ID" ] || fail C3 "CLI create missing id"
    FIRECRAB_API=$API "$FIRECRAB" network delete "$NET_CLI_ID" \
        || fail C3 "firecrab network delete failed"
    NET_CLI_ID=
    pass C3

    host_home=$(mktemp -d)
    HOME=$host_home FIRECRAB_API=$API "$FIRECRAB" host add qa-ci-host "$API" \
        || fail C5 "host add failed"
    HOME=$host_home FIRECRAB_API=$API "$FIRECRAB" host list \
        | grep -q qa-ci-host || fail C5 "host list missing qa-ci-host"
    HOME=$host_home FIRECRAB_API=$API "$FIRECRAB" host use qa-ci-host \
        || fail C5 "host use failed"
    HOME=$host_home FIRECRAB_API=$API "$FIRECRAB" host show qa-ci-host \
        || fail C5 "host show failed"
    HOME=$host_home FIRECRAB_API=$API "$FIRECRAB" host remove qa-ci-host \
        || fail C5 "host remove failed"
    rm -rf "$host_home"
    pass C5
else
    skip C3 "firecrab binary not on PATH"
    skip C5 "firecrab binary not on PATH"
fi

cleanup
NET_ID=
NET_V6_ID=
NET_CLI_ID=
STORAGE_ID=
SHELL_ID=
VM_ID=

leftover_qa /api/vms || fail X1 "leftover qa-* VMs"
pass X1
leftover_qa /api/micro-networks || fail X2 "leftover qa-* networks"
pass X2
leftover_qa /api/micro-storages || fail X3 "leftover qa-* storages"
pass X3
leftover_qa /api/shells || fail X4 "leftover qa-* shells"
pass X4

http GET /api/microregistry/docker-hub
expect X6 200
pass X6
