#!/usr/bin/env bash
# QA guest boot from OCI images (public-docs/qa.md I5, I6, N6, V1, V2, V6,
# V7, V8, V9, V11, V12, V13, P1-P6, C1, C2, C4, C6, X5, X7).
# P1-P6 and C6 run only on the first OCI reference, after guest boot and
# before that reference's image delete. X7 runs at the end of this script.
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
warning() { printf 'WARNING %s: %s\n' "$1" "$2"; }
fail() {
    printf 'FAILED %s: %s\n' "$1" "$2" >&2
    if [ -n "${BODY:-}" ]; then
        printf '%s\n' "$BODY" >&2
    fi
    exit 1
}

CODE=
BODY=
CURRENT_IMPORTED_ALIAS=
POOL_ID=
POOL_NET_ID=
POOL_LEASE_ID=

cleanup_on_exit() {
    # Keep the status that entered EXIT. A cleanup curl must not replace it.
    local status=$?
    local code deadline
    # Release only a lease id this process stored.
    # Do not invent an id that was never observed.
    if [ -n "${POOL_LEASE_ID:-}" ] && [ -n "${POOL_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 30 \
            -X DELETE "${API}/api/pools/${POOL_ID}/leases/${POOL_LEASE_ID}" || true
    fi
    if [ -n "${POOL_ID:-}" ]; then
        # DELETE is accepted before members are gone, and it is refused while
        # a lease is still active. Retry until the row is gone or 60s pass,
        # then drop the network, still without failing the original command.
        deadline=$((SECONDS + 60))
        while true; do
            curl -sS -o /dev/null --max-time 15 \
                -X DELETE "${API}/api/pools/${POOL_ID}" || true
            code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 5 --max-time 15 \
                "${API}/api/pools/${POOL_ID}" || true)
            if [ "$code" = 404 ] || [ "$code" = 000 ] || [ "$SECONDS" -ge "$deadline" ]; then
                break
            fi
            sleep 2
        done
    fi
    if [ -n "${POOL_NET_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 60 \
            -X DELETE "${API}/api/micro-networks/${POOL_NET_ID}" || true
    fi
    # Only aliases imported by this process are eligible for cleanup. A
    # pre-existing catalog/custom image is never put in this variable.
    if [ -n "${CURRENT_IMPORTED_ALIAS:-}" ]; then
        curl -sS -o /dev/null --max-time 60 \
            -X DELETE "${API}/api/images/${CURRENT_IMPORTED_ALIAS}" || true
    fi
    return "$status"
}
trap cleanup_on_exit EXIT

http() {
    local method path body header out
    method=$1
    path=$2
    body=${3-}
    header=${4-}
    out=$(mktemp)
    if [ -n "$body" ] && [ -n "$header" ]; then
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
            -X "$method" \
            -H "Origin: ${API}" \
            -H 'content-type: application/json' \
            -H "$header" \
            --data "$body" \
            "${API}${path}") || CODE=000
    elif [ -n "$body" ]; then
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
            -X "$method" \
            -H "Origin: ${API}" \
            -H 'content-type: application/json' \
            --data "$body" \
            "${API}${path}") || CODE=000
    elif [ -n "$header" ]; then
        CODE=$(curl -sS -o "$out" -w '%{http_code}' --connect-timeout 5 --max-time 60 \
            -X "$method" \
            -H "Origin: ${API}" \
            -H "$header" \
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

pool_create_body() {
    python3 -c 'import json, sys
print(json.dumps({
    "name": sys.argv[1],
    "template": sys.argv[2],
    "cpu": 1,
    "ram": 512,
    "diskGb": int(sys.argv[3]),
    "egressPolicy": "internet",
    "microNetworkId": sys.argv[4],
    "minReady": int(sys.argv[5]),
    "maxSize": int(sys.argv[6]),
    "leaseTtlSeconds": int(sys.argv[7]),
}))' "$@"
}

pool_ready_count() {
    local max_size=$1
    python3 -c '
import json, sys
max_size = int(sys.argv[1])
pool = json.load(sys.stdin)
members = pool.get("members")
if not isinstance(members, list):
    raise SystemExit("members is not a list")
if len(members) > max_size:
    raise SystemExit("members.length %d exceeds maxSize %d" % (len(members), max_size))
print(sum(1 for member in members if member.get("state") == "ready"))
' "$max_size"
}

pool_replacement_phase() {
    local old_vm=$1
    local max_size=$2
    python3 -c '
import json, sys
old = sys.argv[1]
max_size = int(sys.argv[2])
pool = json.load(sys.stdin)
members = pool.get("members")
if not isinstance(members, list):
    raise SystemExit("members is not a list")
if len(members) > max_size:
    raise SystemExit("members.length %d exceeds maxSize %d" % (len(members), max_size))
if any(str(member.get("vmId") or "") == old for member in members):
    print("wait")
    raise SystemExit(0)
ready = [
    str(member.get("vmId") or "")
    for member in members
    if member.get("state") == "ready" and str(member.get("vmId") or "") not in ("", old)
]
print("ready" if ready else "wait")
' "$old_vm" "$max_size"
}

wait_for_ready_member() {
    local id=$1
    local max_size=$2
    local deadline=$3
    local ready_count
    while true; do
        http GET "/api/pools/${id}"
        [ "$CODE" = 200 ] || fail P6 "GET pool HTTP ${CODE}"
        ready_count=$(printf '%s' "$BODY" | pool_ready_count "$max_size") \
            || fail P6 "pool member check failed"
        if [ "$ready_count" -ge 1 ]; then
            return 0
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            fail P6 "timed out waiting for a ready member"
        fi
        sleep 5
    done
}

wait_for_replacement() {
    local id=$1
    local old_vm=$2
    local max_size=$3
    local deadline=$4
    local phase pool_body
    while true; do
        http GET "/api/pools/${id}"
        [ "$CODE" = 200 ] || fail P6 "GET pool HTTP ${CODE}"
        pool_body=$BODY
        phase=$(printf '%s' "$BODY" | pool_replacement_phase "$old_vm" "$max_size") \
            || fail P6 "pool member check failed"
        case "$phase" in
            ready)
                http GET "/api/vms/${old_vm}"
                if [ "$CODE" = 404 ]; then
                    return 0
                fi
                if [ "$CODE" != 200 ]; then
                    fail P6 "GET leased VM HTTP ${CODE}"
                fi
                ;;
            wait) ;;
            *)
                BODY=$pool_body
                fail P6 "unexpected pool poll result"
                ;;
        esac
        if [ "$SECONDS" -ge "$deadline" ]; then
            BODY=$pool_body
            fail P6 "timed out waiting for the leased VM to be replaced"
        fi
        sleep 5
    done
}

delete_pool_row() {
    local id=$1
    local row=$2
    local deadline
    http DELETE "/api/pools/${id}"
    [ "$CODE" = 202 ] || fail "$row" "DELETE pool HTTP ${CODE}"
    [ "$(json_get 'str(bool(d.get("deleting"))).lower()')" = "true" ] \
        || fail "$row" "deleting is not true"
    deadline=$((SECONDS + 60))
    while true; do
        http GET "/api/pools/${id}"
        if [ "$CODE" = 404 ]; then
            if [ "${POOL_ID:-}" = "$id" ]; then
                POOL_ID=
            fi
            return 0
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            fail "$row" "timed out waiting for pool delete"
        fi
        sleep 2
    done
}

# First OCI reference only. The network is not the boot script's subnet.
run_pool_qa() {
    local alias=$1
    local disk=$2
    local body show_json update_json lease_vm lease_vm_id
    case "$disk" in
        ''|*[!0-9]*) fail P1 "diskGb is not an integer (${disk})" ;;
    esac

    http POST /api/micro-networks \
        '{"name":"qa-ci-pool","subnetCidr":"172.31.205.0/24","internetEnabled":true}'
    [ "$CODE" = 201 ] || fail P1 "POST /api/micro-networks HTTP ${CODE}"
    POOL_NET_ID=$(json_get 'd.get("id") or ""')
    [ -n "$POOL_NET_ID" ] || fail P1 "micro-network response missing id"

    body=$(pool_create_body qa-ci-pool "$alias" "$disk" "$POOL_NET_ID" 0 1 600)
    http POST /api/pools "$body"
    [ "$CODE" = 201 ] || fail P1 "POST /api/pools HTTP ${CODE}"
    POOL_ID=$(json_get 'd.get("id") or ""')
    [ -n "$POOL_ID" ] || fail P1 "create response missing id"
    [ -n "$(json_get 'd.get("templateVersion") or ""')" ] \
        || fail P1 "templateVersion is empty"
    [ "$(json_get 'len(d["members"]) if isinstance(d.get("members"), list) else -1')" = 0 ] \
        || fail P1 "members are not empty"
    pass P1

    http GET /api/pools
    [ "$CODE" = 200 ] || fail P2 "GET /api/pools HTTP ${CODE}"
    printf '%s' "$BODY" | python3 -c '
import json, sys
want = sys.argv[1]
rows = json.load(sys.stdin)
if not isinstance(rows, list) or not any(
    isinstance(row, dict) and row.get("id") == want for row in rows
):
    raise SystemExit("list missing pool id")
' "$POOL_ID" || fail P2 "list missing pool id"
    http GET "/api/pools/${POOL_ID}"
    [ "$CODE" = 200 ] || fail P2 "GET pool HTTP ${CODE}"
    [ "$(json_get 'd.get("name") or ""')" = "qa-ci-pool" ] || fail P2 "detail name mismatch"
    pass P2

    show_json=$(qa_cli pool show "$POOL_ID" --json) || {
        BODY=$show_json
        fail C6 "firecrab pool show failed"
    }
    printf '%s' "$show_json" | python3 -c '
import json, sys
if json.load(sys.stdin).get("name") != "qa-ci-pool":
    raise SystemExit("name mismatch")
' || {
        BODY=$show_json
        fail C6 "pool show name mismatch"
    }
    update_json=$(qa_cli pool update "$POOL_ID" --lease-ttl 120 --json) || {
        BODY=$update_json
        fail C6 "firecrab pool update failed"
    }
    printf '%s' "$update_json" | python3 -c '
import json, sys
if json.load(sys.stdin).get("leaseTtlSeconds") != 120:
    raise SystemExit("cli lease ttl")
' || {
        BODY=$update_json
        fail C6 "pool update did not set leaseTtlSeconds 120"
    }
    http GET "/api/pools/${POOL_ID}"
    [ "$CODE" = 200 ] || fail C6 "GET pool after update HTTP ${CODE}"
    [ "$(json_get 'd.get("leaseTtlSeconds")')" = 120 ] || fail C6 "leaseTtlSeconds is not 120"
    pass C6

    http PATCH "/api/pools/${POOL_ID}" '{"minReady":0,"maxSize":1,"leaseTtlSeconds":120}'
    [ "$CODE" = 200 ] || fail P3 "PATCH pool HTTP ${CODE}"
    [ "$(json_get 'd.get("minReady")')" = 0 ] || fail P3 "minReady is not 0"
    [ "$(json_get 'd.get("maxSize")')" = 1 ] || fail P3 "maxSize is not 1"
    [ "$(json_get 'd.get("leaseTtlSeconds")')" = 120 ] || fail P3 "leaseTtlSeconds is not 120"
    pass P3

    http POST "/api/pools/${POOL_ID}/acquire" "" "Idempotency-Key: qa-ci-pool-empty"
    [ "$CODE" = 409 ] || fail P4 "acquire HTTP ${CODE}"
    [ "$(json_get 'd.get("error", {}).get("code") or ""')" = "pool_exhausted" ] \
        || fail P4 "error.code is not pool_exhausted"
    pass P4

    delete_pool_row "$POOL_ID" P5
    pass P5

    # The name is free only after P5 has removed the idle pool.
    body=$(pool_create_body qa-ci-pool "$alias" "$disk" "$POOL_NET_ID" 1 2 600)
    http POST /api/pools "$body"
    [ "$CODE" = 201 ] || fail P6 "POST /api/pools HTTP ${CODE}"
    POOL_ID=$(json_get 'd.get("id") or ""')
    [ -n "$POOL_ID" ] || fail P6 "create response missing id"
    [ "$(json_get 'd.get("minReady")')" = 1 ] || fail P6 "minReady is not 1"
    [ "$(json_get 'd.get("maxSize")')" = 2 ] || fail P6 "maxSize is not 2"
    wait_for_ready_member "$POOL_ID" 2 "$((SECONDS + 300))"

    http POST "/api/pools/${POOL_ID}/acquire"
    [ "$CODE" = 201 ] || fail P6 "acquire HTTP ${CODE}"
    POOL_LEASE_ID=$(json_get 'd.get("id") or ""')
    [ -n "$POOL_LEASE_ID" ] || fail P6 "acquire body missing lease id"
    lease_vm=$(json_get '(d.get("vm") or {}).get("id") or ""') || fail P6 "acquire body missing vm.id"
    lease_vm_id=$(json_get 'd.get("vmId") or ""') || fail P6 "acquire body missing vmId"
    if [ -z "$lease_vm" ]; then
        lease_vm=$lease_vm_id
    fi
    if [ -n "$lease_vm_id" ] && [ "$lease_vm" != "$lease_vm_id" ]; then
        fail P6 "vm.id does not match vmId"
    fi
    [ -n "$lease_vm" ] || fail P6 "acquire body missing vm id"

    http GET /api/vms
    [ "$CODE" = 200 ] || fail P6 "GET /api/vms HTTP ${CODE}"
    printf '%s' "$BODY" | python3 -c '
import json, sys
want = sys.argv[1]
rows = json.load(sys.stdin)
found = [row for row in rows if str(row.get("id") or "") == want]
if not found:
    raise SystemExit("leased VM is not listed")
if found[0].get("purpose") != "pool":
    raise SystemExit("leased VM purpose is not pool")
' "$lease_vm" || fail P6 "leased VM is missing from GET /api/vms"

    http DELETE "/api/vms/${lease_vm}"
    [ "$CODE" = 409 ] || fail P6 "DELETE pool VM HTTP ${CODE}"
    [ "$(json_get 'd.get("error", {}).get("code") or ""')" = "pool_owned" ] \
        || fail P6 "error.code is not pool_owned"

    http DELETE "/api/pools/${POOL_ID}/leases/${POOL_LEASE_ID}"
    [ "$CODE" = 200 ] || fail P6 "release HTTP ${CODE}"
    [ "$(json_get 'd.get("state") or ""')" = "released" ] || fail P6 "lease state is not released"
    POOL_LEASE_ID=

    wait_for_replacement "$POOL_ID" "$lease_vm" 2 "$((SECONDS + 300))"
    delete_pool_row "$POOL_ID" P6

    http DELETE "/api/micro-networks/${POOL_NET_ID}"
    [ "$CODE" = 204 ] || fail P6 "DELETE micro-network HTTP ${CODE}"
    POOL_NET_ID=
    pass P6
}

if [ "$(uname -s)" = Linux ]; then
    test -e /dev/kvm || fail KVM "/dev/kvm is missing"
    if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
        fail KVM "runner cannot read/write /dev/kvm"
    fi
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
    printf '%s\n' 'KVM will be proven by the nested guest boot'
fi

CLI_BIN=${FIRECRAB_QA_CLI:-}
qa_cli() {
    "$CLI_BIN" --api "$API" "$@"
}

boot_oci() {
    local reference=$1
    local first_reference=${2:-0}
    local alias installed disk body imported_image=0 image_list inspect_json
    local import_json import_status_json import_status
    printf 'OCI guest boot reference=%s\n' "$reference"

    if [ "$first_reference" = 1 ]; then
        if [ -z "$CLI_BIN" ]; then
            CLI_BIN=$(command -v firecrab || true)
        fi
        [ -n "$CLI_BIN" ] || fail C4 "firecrab CLI is not on PATH"

        image_list=$(qa_cli image list --json) \
            || fail "C4/${reference}" "firecrab image list failed"
        inspect_json=$(qa_cli image inspect "$reference" --json) \
            || fail "C4/${reference}" "firecrab image inspect failed"
        alias=$(printf '%s' "$inspect_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["alias"])')
        [ -n "$alias" ] || fail "I5/${reference}" "inspect missing alias"
        pass "I5/${reference} alias=${alias} (CLI)"

        installed=$(printf '%s' "$image_list" | python3 -c '
import json, sys
alias = sys.argv[1]
row = next((item for item in json.load(sys.stdin) if item.get("alias") == alias), None)
print(str(bool(row and row.get("installed"))).lower())
' "$alias")
        if [ "$installed" = true ]; then
            # An alias that was present before this run is deliberately not
            # imported or deleted. Report the rows as skipped so an existing
            # image cannot become a false PASS for an import that did not run.
            import_status_json=$(qa_cli image import-status "$alias" --json) \
                || fail "C4/${reference}" "firecrab image import-status failed"
            import_status=$(printf '%s' "$import_status_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
            warning "I6/${reference}" "${alias} was already installed; import not attempted (status=${import_status})"
            warning "C4/${reference}" "${alias} was already installed; import not attempted"
        else
            import_json=$(qa_cli image import "$reference" --json) \
                || fail "I6/${reference}" "firecrab image import failed"
            imported_image=1
            CURRENT_IMPORTED_ALIAS=$alias
            import_status=$(printf '%s' "$import_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
            case "$import_status" in
                idle|running|succeeded) ;;
                failed) fail "I6/${reference}" "firecrab image import returned failed" ;;
                *) fail "I6/${reference}" "unexpected import status ${import_status}" ;;
            esac

            for _ in $(seq 1 240); do
                import_status_json=$(qa_cli image import-status "$alias" --json) \
                    || fail "I6/${reference}" "firecrab image import-status failed"
                import_status=$(printf '%s' "$import_status_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
                case "$import_status" in
                    succeeded) break ;;
                    failed)
                        fail "I6/${reference}" "firecrab image import failed: $(printf '%s' "$import_status_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("log") or "")')"
                        ;;
                    running|idle) sleep 5 ;;
                    *) fail "I6/${reference}" "unexpected import status ${import_status}" ;;
                esac
            done
            [ "$import_status" = succeeded ] || fail "I6/${reference}" "timed out waiting for CLI import"

            image_list=$(qa_cli image list --json) \
                || fail "C4/${reference}" "firecrab image list after import failed"
            installed=$(printf '%s' "$image_list" | python3 -c '
import json, sys
alias = sys.argv[1]
row = next((item for item in json.load(sys.stdin) if item.get("alias") == alias), None)
if not row or row.get("installed") is not True:
    raise SystemExit("imported alias is not installed")
' "$alias") || fail "I6/${reference}" "alias ${alias} not installed"
            pass "I6/${alias} (CLI)"
            pass "C4/${reference} (CLI list/inspect/import/import-status)"
        fi
    else
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
            imported_image=1
            CURRENT_IMPORTED_ALIAS=$alias
            poll_job "I6/${reference}" "/api/oci/import/${alias}"
            http GET "/api/images/${alias}"
            [ "$CODE" = 200 ] || fail "I6/${reference}" "GET image after import HTTP ${CODE}"
            [ "$(json_get 'str(d.get("installed") or False).lower()')" = "true" ] \
                || fail "I6/${reference}" "alias ${alias} not installed"
            pass "I6/${alias}"
        else
            warning "I6/${reference}" "${alias} was already installed; import not attempted"
        fi
    fi

    if [ "$first_reference" = 1 ]; then
        disk=$(printf '%s' "$image_list" | python3 -c '
import json, sys
alias = sys.argv[1]
row = next((item for item in json.load(sys.stdin) if item.get("alias") == alias), None)
print(row.get("minDiskGb") or 2 if row else 2)
' "$alias")
        FIRECRAB_QA_DISK_GB=$disk \
            FIRECRAB_QA_FIRST_REFERENCE=1 \
            FIRECRAB_QA_CLI="$CLI_BIN" \
            "$root/scripts/ci-m2-guest-boot.sh" "$alias"
    else
        disk=$(json_get 'd.get("minDiskGb") or 2')
        FIRECRAB_QA_DISK_GB=$disk "$root/scripts/ci-m2-guest-boot.sh" "$alias"
    fi
    if [ "$first_reference" = 1 ] && [ "$(uname -s)" != Linux ]; then
        pass "KVM nested Firecracker guest boot"
    fi
    pass "V1/${alias}"
    pass "V7/${alias}"
    pass "V8/${alias}"
    pass "V11/${alias}"
    pass "V13/${alias}"

    if [ "$first_reference" = 1 ]; then
        run_pool_qa "$alias" "$disk"
    fi

    if [ "$imported_image" = 1 ]; then
        http DELETE "/api/images/${alias}"
        if [ "$CODE" != 204 ] && [ "$CODE" != 200 ]; then
            fail "X5/${alias}" "DELETE image HTTP ${CODE}"
        fi
        CURRENT_IMPORTED_ALIAS=
        pass "X5/${alias}"
    fi
}

first_reference=1
for REFERENCE in "${REFERENCES[@]}"; do
    boot_oci "$REFERENCE" "$first_reference"
    first_reference=0
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

http GET /api/pools
[ "$CODE" = 200 ] || fail X7 "GET /api/pools HTTP ${CODE}"
printf '%s' "$BODY" | python3 -c '
import json, sys
rows = json.load(sys.stdin)
if not isinstance(rows, list):
    sys.stderr.write("not a JSON list\n")
    sys.exit(1)
bad = [str(row.get("name", "")) for row in rows if str(row.get("name", "")).startswith("qa-")]
if bad:
    sys.stderr.write("leftover pools: " + ", ".join(bad) + "\n")
    sys.exit(1)
' || fail X7 "leftover qa-* pools"
pass X7
