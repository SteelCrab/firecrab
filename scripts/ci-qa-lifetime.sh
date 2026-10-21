#!/usr/bin/env bash
# QA VM lifetime (public-docs/development/qa.md R1–R7, X7): each VM's shim in its own
# systemd unit, a stop from outside the API, a quick restart, an API restart, a
# crash while the API is down, an interrupted start, and unit cleanup.
#
# Usage: scripts/ci-qa-lifetime.sh [OCI reference]   (default alpine:3.21)
#
# The reference's image is imported when its alias is not installed yet, and
# deleted again afterwards (X5); an alias that was already installed is kept.
#
# Root commands run through sudo on Linux, directly in a root WSL shell on
# Windows, or over management VM SSH when FIRECRAB_QA_MANAGER_HOST and
# FIRECRAB_QA_MANAGER_KEY are set (macOS; ci-qa-macos-e2e.sh sets them).
set -euo pipefail

API=${FIRECRAB_API:-http://127.0.0.1:5523}
API=${API%/}
REFERENCE=${1:-alpine:3.21}
WAIT_FACTOR=${FIRECRAB_QA_WAIT_FACTOR:-1}
SUBNET=${FIRECRAB_QA_LIFETIME_SUBNET:-172.219.0.0/24}
HOST_PORT=${FIRECRAB_QA_LIFETIME_PORT:-18081}

pass() { printf 'PASS %s\n' "$1"; }
warning() { printf 'WARNING %s: %s\n' "$1" "$2"; }
fail() {
    printf 'FAILED %s: %s\n' "$1" "$2" >&2
    exit 1
}

# Runs one root shell command on the API host and prints its output.
host() {
    if [ -n "${FIRECRAB_QA_MANAGER_HOST:-}" ]; then
        local key=${FIRECRAB_QA_MANAGER_KEY:?FIRECRAB_QA_MANAGER_KEY is required with FIRECRAB_QA_MANAGER_HOST}
        ssh -i "$key" \
            -o BatchMode=yes \
            -o ConnectTimeout=10 \
            -o StrictHostKeyChecking=accept-new \
            -o "UserKnownHostsFile=$(dirname -- "$key")/known_hosts" \
            "root@${FIRECRAB_QA_MANAGER_HOST}" "set -e; $1" </dev/null
    elif [ "$(id -u)" -eq 0 ]; then
        sh -ec "$1" </dev/null
    else
        sudo sh -ec "$1" </dev/null
    fi
}

api_ready() {
    for _ in $(seq 1 $((60 * WAIT_FACTOR))); do
        curl -fsS --max-time 5 "$API/api/host" >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

vm_state() {
    curl -fsS --max-time 15 "$API/api/vms/$VM" | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"])'
}

# Waits until the VM leaves `starting`/`stopping` and prints the state.
settled_state() {
    local state=
    for _ in $(seq 1 $((120 * WAIT_FACTOR))); do
        state=$(vm_state 2>/dev/null || true)
        case "$state" in
            starting | stopping | '') sleep 2 ;;
            *) break ;;
        esac
    done
    printf '%s\n' "$state"
}

start_vm() {
    curl -fsS -o /dev/null --max-time 300 -X POST "$API/api/vms/$VM/start"
}

shim_pid() { host "pgrep -f '[v]m-shim --vm-id $VM' || true"; }
vmm_pid() { host "pgrep -f '[f]irecracker --api-sock .*/$SIMPLE/' || true"; }
runtime_dir() {
    host "tr '\\0' '\\n' < /proc/$1/cmdline | sed -n '/^--runtime-dir\$/{n;p;}'"
}

json_field() {
    python3 -c 'import json,sys; d=json.load(sys.stdin); v=d.get(sys.argv[1]); print("" if v is None else str(v).lower() if isinstance(v, bool) else v)' "$1"
}

# Prints the alias for REFERENCE, importing it first when it is not installed.
installed_alias() {
    local alias status
    alias=$(curl -fsS --max-time 60 -G "$API/api/oci/inspect" --data-urlencode "reference=$REFERENCE" | json_field alias)
    [ -n "$alias" ] || fail R1 "no alias for $REFERENCE"
    if [ "$(curl -sS --max-time 30 "$API/api/images/$alias" | json_field installed 2>/dev/null)" != true ]; then
        curl -fsS -o /dev/null --max-time 60 -X POST "$API/api/oci/import" \
            -H 'content-type: application/json' \
            -d "$(python3 -c 'import json,sys; print(json.dumps({"reference": sys.argv[1]}))' "$REFERENCE")"
        IMPORTED=$alias
        for _ in $(seq 1 240); do
            status=$(curl -fsS --max-time 30 "$API/api/oci/import/$alias" | json_field status)
            case "$status" in
                succeeded) break ;;
                failed) fail R1 "importing $REFERENCE failed" ;;
                *) sleep 5 ;;
            esac
        done
        [ "$status" = succeeded ] || fail R1 "timed out importing $REFERENCE"
    fi
    TEMPLATE=$alias
}

NET=
VM=
IMPORTED=
KEY=
KNOWN_HOSTS=
API_STOPPED=0
HELPER_STOPPED=0
HELPER_BLOCK=
SENTINEL=
IP_FORWARD_SAVED=
cleanup() {
    if [ -n "$HELPER_BLOCK" ]; then host "rm -f '$HELPER_BLOCK'; rmdir /run/systemd/system/firecrab-helper.service.d 2>/dev/null || true; systemctl daemon-reload" || true; fi
    if [ "$HELPER_STOPPED" = 1 ]; then host "systemctl start firecrab-helper" || true; fi
    if [ "$API_STOPPED" = 1 ]; then host "systemctl start firecrab-api" || true; api_ready || true; fi
    # Host-wide: put back what R4c found, whichever way the run ended.
    case "$IP_FORWARD_SAVED" in
        0 | 1) host "sysctl -w net.ipv4.ip_forward=$IP_FORWARD_SAVED >/dev/null" || true ;;
    esac
    if [ -n "$SENTINEL" ]; then host "nft delete table inet $SENTINEL" || true; fi
    if [ -n "$KEY" ]; then rm -f "$KEY"; fi
    if [ -n "$KNOWN_HOSTS" ]; then rm -f "$KNOWN_HOSTS"; fi
    if [ -n "${VM:-}" ]; then
        curl -sS -o /dev/null --max-time 120 -X POST "$API/api/vms/$VM/stop" || true
        curl -sS -o /dev/null --max-time 30 -X DELETE "$API/api/vms/$VM" || true
    fi
    if [ -n "${NET:-}" ]; then
        curl -sS -o /dev/null --max-time 15 -X DELETE "$API/api/micro-networks/$NET" || true
    fi
    if [ -n "${IMPORTED:-}" ]; then
        curl -sS -o /dev/null --max-time 60 -X DELETE "$API/api/images/$IMPORTED" || true
    fi
}
trap cleanup EXIT

installed_alias
printf 'VM lifetime: template=%s api=%s\n' "$TEMPLATE" "$API"

NET=$(curl -fsS -X POST "$API/api/micro-networks" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"qa-lifetime-$$\",\"subnetCidr\":\"$SUBNET\",\"internetEnabled\":true}" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
VM=$(curl -fsS -X POST "$API/api/vms" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"qa-lifetime-$$\",\"template\":\"$TEMPLATE\",\"cpu\":1,\"ram\":${FIRECRAB_QA_RAM:-512},\"diskGb\":${FIRECRAB_QA_DISK_GB:-2},\"microNetworkId\":\"$NET\",\"portForwards\":[{\"hostPort\":$HOST_PORT,\"guestPort\":80,\"protocol\":\"tcp\"}]}" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
SIMPLE=$(printf '%s' "$VM" | tr -d -)
TAP=$(python3 -c 'import hashlib, sys, uuid; print("fct" + hashlib.sha256(uuid.UUID(sys.argv[1]).bytes).hexdigest()[:12])' "$VM")
UNIT="firecrab-vm-$SIMPLE.service"

start_vm
[ "$(settled_state)" = running ] || fail R1 "VM did not reach running"

# R1: Firecracker runs under this VM's shim, in the VM's own unit, owned by
# PID 1 and run as the API user, under the ceilings the API derives from the
# VM's RAM and vCPUs: RAM plus max(256 MiB, RAM/8), and one core per vCPU
# plus one (this VM has one vCPU, so 200%).
SHIM=$(shim_pid)
VMM=$(vmm_pid)
[ -n "$SHIM" ] || fail R1 "no shim for $VM"
[ -n "$VMM" ] || fail R1 "no Firecracker for $VM"
[ "$(host "ps -o ppid= -p $VMM" | tr -d ' ')" = "$SHIM" ] || fail R1 "Firecracker $VMM is not a child of shim $SHIM"
RUNTIME=$(runtime_dir "$SHIM")
host "test -S '$RUNTIME/shim.sock' && test -f '$RUNTIME/console.log'" || fail R1 "runtime $RUNTIME lacks shim.sock or console.log"
[ "$(host "systemctl is-active $UNIT" || true)" = active ] || fail R1 "$UNIT is not active"
[ "$(host "ps -o ppid= -p $SHIM" | tr -d ' ')" = 1 ] || fail R1 "shim $SHIM is not a child of PID 1"
api_user=$(host "systemctl show -p User --value firecrab-api")
[ "$(host "ps -o user= -p $SHIM" | tr -d ' ')" = "${api_user:-root}" ] || fail R1 "shim does not run as ${api_user:-root}"
RAM=${FIRECRAB_QA_RAM:-512}
ALLOWANCE=$((RAM / 8))
[ "$ALLOWANCE" -ge 256 ] || ALLOWANCE=256
WANT_MEMORY_MAX=$(((RAM + ALLOWANCE) * 1024 * 1024))
[ "$(host "systemctl show -p MemoryMax --value $UNIT")" = "$WANT_MEMORY_MAX" ] || fail R1 "$UNIT MemoryMax is not $WANT_MEMORY_MAX bytes"
[ "$(host "systemctl show -p CPUQuotaPerSecUSec --value $UNIT")" = 2s ] || fail R1 "$UNIT CPUQuota is not 200%"
pass "R1 shim $SHIM owns Firecracker $VMM in $UNIT, parent PID 1, user ${api_user:-root}, ceilings $((WANT_MEMORY_MAX / 1048576)) MiB and 200% CPU"

# R2: a stop from outside the API (`systemctl stop`, as at host shutdown)
# records stopped, whatever signal ended Firecracker.
host "systemctl stop $UNIT"
[ "$(settled_state)" = stopped ] || fail R2 "VM is $(vm_state) after systemctl stop, expected stopped"
host "grep -q '\"stop_requested\":true' '$RUNTIME/exit.json'" || fail R2 "exit.json does not record the requested stop"
host "! ip link show $TAP >/dev/null 2>&1" || fail R2 "$TAP is still present"
pass "R2 systemctl stop recorded stopped and removed $TAP"

# R3: a start right after a stop reuses the unit name without waiting for the
# previous unit to be collected.
start_vm
[ "$(settled_state)" = running ] || fail R3 "VM did not reach running again"
curl -fsS -o /dev/null --max-time 120 -X POST "$API/api/vms/$VM/stop"
start_vm
[ "$(settled_state)" = running ] || fail R3 "VM is $(vm_state) after a stop and an immediate start, expected running"
SHIM=$(shim_pid)
VMM=$(vmm_pid)
pass "R3 a start right after a stop reached running"

# Establish real guest traffic before testing lifetime/recovery. The injected
# static toolbox provides httpd for the default Alpine fixture.
IPV4=$(curl -fsS "$API/api/vms/$VM" | json_field ipv4)
DETAIL=$(curl -fsS "$API/api/micro-networks/$NET")
BRIDGE=$(printf '%s' "$DETAIL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bridge"]["name"])')
GATEWAY=$(printf '%s' "$DETAIL" | python3 -c 'import json,sys; print(json.load(sys.stdin)["subnet"]["gateway"])')
PREFIX=${SUBNET#*/}
BRIDGE_MTU=$(host "cat /sys/class/net/$BRIDGE/mtu")
KEY=$(mktemp)
KNOWN_HOSTS=$(mktemp)
chmod 600 "$KEY" "$KNOWN_HOSTS"
curl -fsS "$API/api/vms/$VM/ssh-key" > "$KEY"
PUBLIC_KEY=$(curl -fsS "$API/api/vms/$VM/ssh-host-key" | json_field publicKey)
printf '%s %s\n' "$IPV4" "$PUBLIC_KEY" > "$KNOWN_HOSTS"
SSH_TRANSPORT=()
if [ -n "${FIRECRAB_QA_MANAGER_HOST:-}" ]; then
    printf -v proxy '%q ' ssh -i "$FIRECRAB_QA_MANAGER_KEY" -o BatchMode=yes \
        -o StrictHostKeyChecking=accept-new \
        -o "UserKnownHostsFile=$(dirname -- "$FIRECRAB_QA_MANAGER_KEY")/known_hosts" \
        "root@$FIRECRAB_QA_MANAGER_HOST"
    proxy+='-W %h:%p'
    SSH_TRANSPORT=(-o "ProxyCommand=$proxy")
fi
guest() {
    ssh "${SSH_TRANSPORT[@]}" -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
        -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN_HOSTS" \
        -o ConnectTimeout=5 "root@$IPV4" "$@"
}
for _ in $(seq 1 $((60 * WAIT_FACTOR))); do
    guest true >/dev/null 2>&1 && break
    sleep 2
done
guest "mkdir -p /tmp/qa-lifetime; printf '%s\n' '$VM' > /tmp/qa-lifetime/index.html; /etc/firecrab/busybox httpd -p 80 -h /tmp/qa-lifetime" \
    || fail R4 "guest HTTP fixture could not start"
traffic() {
    host "ping -c 1 -W 3 $IPV4 >/dev/null && test \"\$(curl -fsS --max-time 5 http://127.0.0.1:$HOST_PORT/)\" = '$VM'" || return 1
    guest true >/dev/null
}
traffic || fail R4 "guest ping/SSH/forwarded HTTP failed before API restart"
check_recovered() {
    local id=$1
    api_ready || fail "$id" "API did not come back"
    [ "$(shim_pid)" = "$SHIM" ] || fail "$id" "shim PID changed"
    [ "$(vmm_pid)" = "$VMM" ] || fail "$id" "Firecracker PID changed"
    [ "$(vm_state)" = running ] || fail "$id" "VM is not running"
    curl -fsS "$API/api/vms/$VM" | python3 -c 'import json,sys; assert json.load(sys.stdin)["reconciliation"]["outcome"] == "reconnected"' \
        || fail "$id" "network recovery did not report success"
    host "ip -o link show $TAP | grep -q 'master $BRIDGE'" || fail "$id" "TAP attachment is wrong"
    host "ip -o addr show dev $BRIDGE | grep -q '$GATEWAY/$PREFIX'" || fail "$id" "gateway prefix is wrong"
    host "nft list chain inet firecrab vm_${SIMPLE}_dnat_out | grep -q '$IPV4:80'" || fail "$id" "DNAT policy is absent"
    host "nft list map inet firecrab vm_egress | grep -q '$IPV4'" || fail "$id" "egress dispatch is absent"
    host "nft list table netdev firecrab_l2_$SIMPLE | grep -q 'ether saddr'" || fail "$id" "anti-spoofing policy is absent"
    traffic || fail "$id" "guest ping/SSH/forwarded HTTP failed"
}
stop_api() { API_STOPPED=1; host "systemctl stop firecrab-api"; }
# Failure injections deliberately restart several times inside systemd's
# default start-limit window; reset that QA-only counter before each start.
start_api() { host "systemctl reset-failed firecrab-api; systemctl start firecrab-api"; api_ready || fail R4 "API did not come back"; API_STOPPED=0; }

# R4: an API restart re-adopts the running VM.
host "systemctl restart firecrab-api"
api_ready || fail R4 "API did not come back"
[ "$(shim_pid)" = "$SHIM" ] || fail R4 "the shim PID changed across the restart"
[ "$(vmm_pid)" = "$VMM" ] || fail R4 "the Firecracker PID changed across the restart"
[ "$(settled_state)" = running ] || fail R4 "VM is $(vm_state) after the restart, expected running"
host "journalctl -u firecrab-api -b --no-pager | grep 'startup reconciliation finished' | tail -1" |
    grep -Eq 'adopted=[1-9]' || fail R4 "the last reconciliation adopted no VM"
check_recovered R4
if command -v firecrab >/dev/null 2>&1; then
    # Ctrl+] detaches; reaching it proves the console WebSocket attached.
    printf '\035' | firecrab --api "$API" vm console "$VM" >/dev/null 2>&1 || fail R4 "console did not attach after adoption"
    pass "R4 API restart kept shim $SHIM, re-adopted the VM, kept $TAP, and console works"
else
    pass "R4 API restart kept shim $SHIM, re-adopted the VM, and kept $TAP"
    warning R4 "firecrab CLI not on PATH; console after adoption not checked"
fi

# DHCP and forwarding drift are host-wide. Run these injections only on a
# disposable test host with no other active VMs.
curl -fsS "$API/api/vms" | python3 -c 'import json,sys; own=sys.argv[1]; assert not any(vm["id"] != own and vm["state"] in ("starting","running","stopping") for vm in json.load(sys.stdin))' "$VM" \
    || fail R4b "network drift QA requires a host with no other active VMs"

# R4f is opt-in: installer migration runs only on a disposable local CI host.
if [ "${FIRECRAB_QA_HELPER_UPGRADE:-0}" = 1 ]; then
    sudo -E scripts/ci-qa-helper-upgrade.sh
    api_ready || fail R4f "API did not return after helper migration"
    check_recovered R4f
    pass "R4f legacy helper migration kept VM PIDs and guest traffic"
fi

# R4b: nft drift while the helper remains alive. Only this QA VM's
# policy is damaged; a foreign table must survive the owned-table replay.
SENTINEL=firecrab_qa_$SIMPLE
host "nft add table inet $SENTINEL"
stop_api
host "nft flush chain inet firecrab vm_${SIMPLE}_dnat_out; nft delete element inet firecrab vm_egress '{ $IPV4 }'; nft flush table netdev firecrab_l2_$SIMPLE"
start_api
check_recovered R4b
host "nft list table inet $SENTINEL >/dev/null" || fail R4b "unrelated host table was changed"
pass "R4b owned nft drift repaired with the helper alive; foreign table preserved"

# R4c: a detached/down TAP and wrong bridge link/address configuration.
# IPv4 forwarding is host-wide: save its value before breaking it so cleanup
# can restore it even when this step fails.
IP_FORWARD_SAVED=$(host "cat /proc/sys/net/ipv4/ip_forward")
stop_api
host "ip link set $TAP nomaster; ip link set $TAP down; ip link set $BRIDGE down; ip link set $BRIDGE mtu 1300; ip addr del $GATEWAY/$PREFIX dev $BRIDGE; ip addr add $GATEWAY/25 dev $BRIDGE; sysctl -w net.ipv4.ip_forward=0 >/dev/null"
start_api
check_recovered R4c
[ "$(host "cat /sys/class/net/$BRIDGE/mtu")" = "$BRIDGE_MTU" ] || fail R4c "bridge MTU was not restored"
host "test \"\$(cat /proc/sys/net/ipv4/ip_forward)\" = 1" || fail R4c "IPv4 forwarding was not restored"
pass "R4c TAP, bridge state, MTU, gateway prefix and forwarding repaired"

# R4d: same DHCP revision, corrupt files, and a crashed serving process.
stop_api
host "printf 'invalid\n' > /run/firecrab/dnsmasq-hosts.conf; printf 'interface=lo\n' > /run/firecrab/dnsmasq.conf; kill -9 \"\$(cat /run/firecrab/dnsmasq.pid)\""
start_api
check_recovered R4d
host "grep -q '$IPV4' /run/firecrab/dnsmasq-hosts.conf; grep -q 'interface=$BRIDGE' /run/firecrab/dnsmasq.conf; kill -0 \"\$(cat /run/firecrab/dnsmasq.pid)\"" || fail R4d "serving DHCP snapshot was not restored"
guest '/etc/firecrab/busybox udhcpc -i eth0 -n -q -t 3 -T 2 -s /etc/firecrab/dhcp.script' || fail R4d "guest could not obtain its reserved address again"
traffic || fail R4d "traffic failed after DHCP renewal"
pass "R4d DHCP config/reservations and serving process repaired at unchanged revision"

# R4e: persistent helper outage is observable; explicit retry recovers it
# without restarting the API, shim, or Firecracker.
stop_api
HELPER_STOPPED=1
# firecrab-api Wants the helper, so starting it would otherwise undo our
# outage. A private runtime drop-in holds off dependency activation only
# for this test and is removed before recovery (including on failure).
HELPER_BLOCK=/run/systemd/system/firecrab-helper.service.d/qa-lifetime-$SIMPLE.conf
host "mkdir -p /run/systemd/system/firecrab-helper.service.d; printf '[Unit]\nConditionPathExists=/run/firecrab-qa-helper-$SIMPLE.enabled\n' > '$HELPER_BLOCK'; systemctl daemon-reload; systemctl stop firecrab-helper"
start_api
[ "$(host "systemctl is-active firecrab-helper" || true)" != active ] || fail R4e "helper outage was not held"
curl -fsS "$API/api/vms/$VM" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["state"] == "running" and d["reconciliation"]["outcome"] == "networkFailed" and d["reconciliation"]["detail"]' || fail R4e "helper failure was not visible"
[ "$(curl -sS -o /dev/null -w '%{http_code}' --max-time 60 -X POST "$API/api/network/reconcile")" = 503 ] || fail R4e "retry during helper outage should fail with 503"
API_RETRY_PID=$(host "systemctl show -p MainPID --value firecrab-api")
host "rm -f '$HELPER_BLOCK'; rmdir /run/systemd/system/firecrab-helper.service.d 2>/dev/null || true; systemctl daemon-reload; systemctl start firecrab-helper"
HELPER_BLOCK=
HELPER_STOPPED=0
curl -fsS -o /dev/null --max-time 60 -X POST "$API/api/network/reconcile" || fail R4e "operator network retry failed"
check_recovered R4e
[ "$(host "systemctl show -p MainPID --value firecrab-api")" = "$API_RETRY_PID" ] || fail R4e "API PID changed during operator retry"
pass "R4e helper outage reported; operator retry restored networking with unchanged VM PIDs"
host "nft delete table inet $SENTINEL"
SENTINEL=

# R5: a crash while the API is down is recorded from exit.json. The VM's
# address stays reserved for it until it is deleted.
host "systemctl stop firecrab-api; kill -9 $VMM; sleep 2; systemctl start firecrab-api"
api_ready || fail R5 "API did not come back"
[ "$(settled_state)" = error ] || fail R5 "VM is $(vm_state) after a crash, expected error"
host "! ip link show $TAP >/dev/null 2>&1" || fail R5 "$TAP is still present"
host "! nft list ruleset 2>/dev/null | grep -q '${VM%%-*}'" || fail R5 "nft rules for $VM are still present"
[ "$(curl -fsS --max-time 15 "$API/api/vms/$VM" | json_field ipv4)" = "$IPV4" ] || fail R5 "$VM lost its address $IPV4 after the crash"
pass "R5 crash while the API was down recorded error, removed $TAP and kept $IPV4"

# R6: a start the restart cuts short is killed and recorded as an error.
start_vm
host "for i in \$(seq 1 200); do pgrep -f '[v]m-shim --vm-id $VM' >/dev/null && break; sleep 0.05; done; systemctl restart firecrab-api"
api_ready || fail R6 "API did not come back"
state=$(settled_state)
if [ "$state" = running ]; then
    warning R6 "the VM reached running before the restart; the interrupted start was not exercised"
else
    [ "$state" = error ] || fail R6 "VM is $state after an interrupted start, expected error"
    sleep 2
    [ "$(host "systemctl is-active $UNIT" || true)" != active ] || fail R6 "$UNIT still active"
    pass "R6 interrupted start recorded error and left no unit"
fi

# R7: a normal stop records stopped and leaves no failed unit.
if [ "$(vm_state)" != running ]; then
    start_vm
    [ "$(settled_state)" = running ] || fail R7 "VM did not reach running"
fi
RUNTIME=$(runtime_dir "$(shim_pid)")
curl -fsS -o /dev/null --max-time 120 -X POST "$API/api/vms/$VM/stop"
[ "$(settled_state)" = stopped ] || fail R7 "VM is $(vm_state) after stop, expected stopped"
host "grep -q '\"stop_requested\":true' '$RUNTIME/exit.json'" || fail R7 "exit.json does not record the requested stop"
host "! systemctl --failed --plain --no-legend | grep -q '$UNIT'" || fail R7 "$UNIT is failed"
pass "R7 stop recorded stopped and left no failed unit"

curl -fsS -o /dev/null --max-time 30 -X DELETE "$API/api/vms/$VM"
VM=
curl -fsS -o /dev/null --max-time 15 -X DELETE "$API/api/micro-networks/$NET"
NET=
[ -z "$(host "systemctl list-units --all --plain --no-legend '$UNIT'")" ] || fail X7 "$UNIT remains"
if [ -n "$IMPORTED" ]; then
    curl -fsS -o /dev/null --max-time 60 -X DELETE "$API/api/images/$IMPORTED"
    IMPORTED=
fi
pass "X7 no unit remains"
