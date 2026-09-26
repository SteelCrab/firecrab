#!/usr/bin/env bash
# OCI nginx: port-forward, VM env, and Shell repository pin (QA V3–V5, V8, V10).
# Usage: scripts/ci-qa-nginx.sh [nginx:1.27-alpine]
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
REFERENCE=${1:-nginx:1.27-alpine}
HOST_PORT=${FIRECRAB_QA_NGINX_PORT:-18080}
# Hosts with slow guest first boots (a nested lab) scale every guest wait.
WAIT_FACTOR=${FIRECRAB_QA_WAIT_FACTOR:-1}
SUBNET=172.31.221.0/24
PREFIX=qa-ngx-

pass() { printf 'PASS %s\n' "$1"; }
warning() { printf 'WARNING %s\n' "$1"; }
fail() {
    printf 'FAILED %s: %s\n' "$1" "$2" >&2
    if [ -n "${BODY:-}" ]; then
        printf '%s\n' "$BODY" >&2
    fi
    exit 1
}

CODE=
BODY=
NET_ID=
VM_ID=
SHELL_ID=
ALIAS=
KEY=
KNOWN_HOSTS=
IMPORTED_IMAGE=0
SSH_TRANSPORT=()

cleanup() {
    if [ -n "${VM_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 120 -X POST "${API}/api/vms/${VM_ID}/stop" || true
        curl -sS -o /dev/null --max-time 30 -X DELETE "${API}/api/vms/${VM_ID}" || true
    fi
    if [ -n "${NET_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/micro-networks/${NET_ID}" || true
    fi
    if [ -n "${SHELL_ID:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/shells/${SHELL_ID}" || true
    fi
    if [ "${IMPORTED_IMAGE:-0}" = 1 ] && [ -n "${ALIAS:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "${API}/api/images/${ALIAS}" || true
    fi
    if [ -n "${KEY:-}" ]; then
        rm -f "$KEY"
    fi
    if [ -n "${KNOWN_HOSTS:-}" ]; then
        rm -f "$KNOWN_HOSTS"
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

wait_running() {
    local id=$1
    local state
    for _ in $(seq 1 60); do
        http GET "/api/vms/${id}"
        [ "$CODE" = 200 ] || fail V7 "GET vm HTTP ${CODE}"
        state=$(json_get 'd.get("state") or ""')
        printf 'state=%s\n' "$state"
        case "$state" in
            running) return 0 ;;
            error | stopped) fail V7 "guest entered ${state} before running" ;;
        esac
        sleep 5
    done
    fail V7 "timed out waiting for running"
}

configure_guest_transport() {
    local manager_host=${FIRECRAB_QA_MANAGER_HOST:-}
    local manager_key=${FIRECRAB_QA_MANAGER_KEY:-}
    local manager_known_hosts proxy
    local -a proxy_command

    if [ "$(uname -s)" = Linux ]; then
        return
    fi
    if [ -z "$manager_host" ] || [ -z "$manager_key" ]; then
        [ "${FIRECRAB_QA_REQUIRE_LIVE_GUEST:-0}" != 1 ] \
            || fail V8 "management VM SSH proxy is required for nginx live checks"
        return
    fi
    [ -r "$manager_key" ] || fail V8 "management VM SSH key is not readable: ${manager_key}"
    manager_known_hosts="$(dirname -- "$manager_key")/known_hosts"
    proxy_command=(
        /usr/bin/ssh -i "$manager_key"
        -o BatchMode=yes
        -o ConnectTimeout=5
        -o StrictHostKeyChecking=accept-new
        -o "UserKnownHostsFile=$manager_known_hosts"
        "root@$manager_host"
    )
    printf -v proxy '%q ' "${proxy_command[@]}"
    proxy+='-W %h:%p'
    SSH_TRANSPORT=(-o "ProxyCommand=$proxy")
}

manager_curl_status() {
    local manager_host=${FIRECRAB_QA_MANAGER_HOST:-}
    local manager_key=${FIRECRAB_QA_MANAGER_KEY:-}
    local manager_known_hosts
    manager_known_hosts="$(dirname -- "$manager_key")/known_hosts"
    ssh -i "$manager_key" \
        -o BatchMode=yes \
        -o ConnectTimeout=5 \
        -o StrictHostKeyChecking=accept-new \
        -o "UserKnownHostsFile=$manager_known_hosts" \
        "root@$manager_host" \
        curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 \
        "http://127.0.0.1:${HOST_PORT}/"
}

guest_ssh() {
    local ipv4=$1
    shift
    ssh "${SSH_TRANSPORT[@]}" -i "$KEY" \
        -o StrictHostKeyChecking=yes \
        -o UserKnownHostsFile="$KNOWN_HOSTS" \
        -o IdentitiesOnly=yes \
        -o ConnectTimeout=5 \
        -o BatchMode=yes \
        "root@${ipv4}" "$@"
}

read_guest_qa_nginx() {
    local ipv4=$1
    guest_ssh "$ipv4" ". /etc/firecrab/vm.env && printf \"%s\\n\" \"\$QA_NGINX\""
}

printf 'nginx OCI reference=%s\n' "$REFERENCE"

inspect_ref "$REFERENCE"
[ "$CODE" = 200 ] || fail I5 "inspect HTTP ${CODE}"
ALIAS=$(json_get 'd["alias"]')
[ -n "$ALIAS" ] || fail I5 "inspect missing alias"
pass "I5 alias=${ALIAS}"

http GET "/api/images/${ALIAS}"
installed=false
if [ "$CODE" = 200 ]; then
    installed=$(json_get 'str(d.get("installed") or False).lower()')
fi
if [ "$installed" != "true" ]; then
    body=$(python3 -c 'import json,sys; print(json.dumps({"reference":sys.argv[1]}))' "$REFERENCE")
    http POST /api/oci/import "$body"
    if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
        fail I6 "POST /api/oci/import HTTP ${CODE}"
    fi
    IMPORTED_IMAGE=1
    poll_job I6 "/api/oci/import/${ALIAS}"
    http GET "/api/images/${ALIAS}"
    [ "$CODE" = 200 ] || fail I6 "GET image after import HTTP ${CODE}"
    [ "$(json_get 'str(d.get("installed") or False).lower()')" = "true" ] \
        || fail I6 "alias ${ALIAS} not installed"
    pass I6
else
    warning "I6 reused installed alias=${ALIAS}; no import occurred"
fi
DISK=$(json_get 'd.get("minDiskGb") or 2')

http POST /api/micro-networks \
    "{\"name\":\"${PREFIX}net\",\"subnetCidr\":\"${SUBNET}\",\"internetEnabled\":true}"
[ "$CODE" = 201 ] || fail N1 "create network HTTP ${CODE}"
NET_ID=$(json_get 'd["id"]')

SHELL_BODY=$(python3 -c 'import json; print(json.dumps({
  "name": "qa-ngx-hello",
  "description": "ci nginx shell pin",
  "content": "#!/bin/sh\necho FIRECRAB_QA_SHELL_OK\ntouch /run/firecrab-qa-shell-ok\n"
}))')
http POST /api/shells "$SHELL_BODY"
[ "$CODE" = 201 ] || fail L1 "create shell HTTP ${CODE}"
SHELL_ID=$(json_get 'd["shellId"]')
[ -n "$SHELL_ID" ] || fail L1 "missing shellId"
pass L1

CREATE=$(python3 -c 'import json,sys
print(json.dumps({
  "name": "qa-ngx-1",
  "template": sys.argv[1],
  "cpu": 1,
  "ram": 1024,
  "diskGb": int(sys.argv[2]),
  "microNetworkId": sys.argv[3],
  "egressPolicy": "internet",
  "env": {"QA_NGINX": "ci"},
  "shellIds": [sys.argv[4]],
  "portForwards": [{"hostPort": int(sys.argv[5]), "guestPort": 80, "protocol": "tcp"}],
}))' "$ALIAS" "$DISK" "$NET_ID" "$SHELL_ID" "$HOST_PORT")
http POST /api/vms "$CREATE"
[ "$CODE" = 201 ] || fail V1 "create vm HTTP ${CODE}"
VM_ID=$(json_get 'd["id"]')
[ "$(json_get 'd.get("env",{}).get("QA_NGINX") or ""')" = "ci" ] || fail V3 "create env QA_NGINX missing"
pass V1
pass "V3 create response preserved QA_NGINX"

http POST "/api/vms/${VM_ID}/start"
if [ "$CODE" != 200 ] && [ "$CODE" != 202 ]; then
    fail V7 "POST start HTTP ${CODE}"
fi
wait_running "$VM_ID"
IPV4=$(json_get 'd.get("ipv4") or ""')
[ -n "$IPV4" ] || fail V7 "running vm missing ipv4"
pass V7
printf 'ipv4=%s\n' "$IPV4"

configure_guest_transport
http_ok=0
for _ in $(seq 1 $((30 * WAIT_FACTOR))); do
    if [ "$(uname -s)" = Linux ]; then
        pf=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 \
            "http://127.0.0.1:${HOST_PORT}/" || true)
    elif [ ${#SSH_TRANSPORT[@]} -gt 0 ]; then
        pf=$(manager_curl_status || true)
    else
        pf=
    fi
    if [ "$pf" = 200 ]; then
        http_ok=1
        break
    fi
    sleep 2
done
[ "$http_ok" = 1 ] || fail V4 "management VM port-forward :${HOST_PORT} did not return 200"
pass "V4 live curl :${HOST_PORT}"
if [ "$(uname -s)" = Darwin ]; then
    # microManager relays running VMs' TCP forwards onto the Mac's loopback.
    http_ok=0
    for _ in $(seq 1 $((15 * WAIT_FACTOR))); do
        pf=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 \
            "http://127.0.0.1:${HOST_PORT}/" || true)
        if [ "$pf" = 200 ]; then
            http_ok=1
            break
        fi
        sleep 2
    done
    [ "$http_ok" = 1 ] || fail V4 "macOS loopback port-forward :${HOST_PORT} did not return 200"
    pass "V4 macOS loopback curl :${HOST_PORT}"
fi

KEY=$(mktemp)
chmod 600 "$KEY"
KNOWN_HOSTS=$(mktemp)
chmod 600 "$KNOWN_HOSTS"
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
FIRECRAB_API=$API FIRECRAB_QA_SSH_KEY=$KEY FIRECRAB_QA_KNOWN_HOSTS=$KNOWN_HOSTS \
    "$root/scripts/ci-qa-ssh.sh" "$VM_ID" "$IPV4"

if [ "$(uname -s)" = Linux ] || [ ${#SSH_TRANSPORT[@]} -gt 0 ]; then
    ssh_ok=0
    for _ in $(seq 1 $((30 * WAIT_FACTOR))); do
        if guest_ssh "$IPV4" true >/dev/null 2>&1; then
            ssh_ok=1
            break
        fi
        sleep 3
    done
    [ "$ssh_ok" = 1 ] || fail V8 "ssh root@${IPV4} never became ready"
    env_value=$(read_guest_qa_nginx "$IPV4") \
        || fail V3 "guest /etc/firecrab/vm.env could not be sourced"
    [ "$env_value" = ci ] \
        || fail V3 "guest /etc/firecrab/vm.env missing QA_NGINX=ci (got ${env_value:-missing})"
    pass "V3 guest vm.env"
    guest_ssh "$IPV4" test -x /var/lib/firecrab/shells/00.sh \
        || fail V5 "guest missing pinned /var/lib/firecrab/shells/00.sh"
    pass "V5 guest shell pin"

    PUT=$(python3 -c 'import json,sys
print(json.dumps({
  "cpu": 1,
  "ram": 1024,
  "diskGb": int(sys.argv[1]),
  "env": {"QA_NGINX": "two"},
}))' "$DISK")
    http PUT "/api/vms/${VM_ID}" "$PUT"
    [ "$CODE" = 200 ] || fail V10 "PUT env while running HTTP ${CODE}"
    # The API pushes vm.env to a running guest in the background; a busy
    # guest (first-boot package installs) can take more than a moment.
    env_value=
    for _ in $(seq 1 $((15 * WAIT_FACTOR))); do
        sleep 2
        env_value=$(read_guest_qa_nginx "$IPV4") \
            || fail V10 "guest /etc/firecrab/vm.env could not be sourced after update"
        [ "$env_value" = two ] && break
    done
    [ "$env_value" = two ] \
        || fail V10 "guest vm.env not updated to QA_NGINX=two (got ${env_value:-missing})"
    pass V10
else
    warning "NGX6 skip live ssh on $(uname -s)"
    warning "NGX6b skip guest env check on $(uname -s)"
    warning "NGX7 skip guest shell check on $(uname -s)"
    warning "NGX8 skip running env update on $(uname -s)"
fi

http POST "/api/vms/${VM_ID}/stop"
wait_stop=0
for _ in $(seq 1 24); do
    http GET "/api/vms/${VM_ID}"
    st=$(json_get 'd.get("state") or ""')
    if [ "$st" = stopped ]; then
        wait_stop=1
        break
    fi
    sleep 5
done
[ "$wait_stop" = 1 ] || fail V11 "vm did not stop"
pass V11

http DELETE "/api/vms/${VM_ID}"
[ "$CODE" = 204 ] || [ "$CODE" = 200 ] || fail V13 "DELETE vm HTTP ${CODE}"
VM_ID=
pass V13
