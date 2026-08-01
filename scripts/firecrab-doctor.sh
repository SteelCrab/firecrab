#!/usr/bin/env bash
# firecrab host doctor — diagnose the host-config failures that look like
# product bugs (UFW, helper socket, wrong cwd/DB, KVM, nft, dnsmasq).
#
# Safe to run unprivileged: items that need root are reported as SKIP with
# a one-line fix. Changes nothing on the host.
#
# Usage: ./scripts/firecrab-doctor.sh [--digest] [-h|--help]
#        ./install.sh --doctor
#        firecrab-doctor   (after install)
#
# See docs/30-tasks/task-host-doctor.md
set -Eeuo pipefail

DIGEST=0
DATADIR=${DATADIR:-/var/lib/firecrab}
HELPER_SOCK=${FIRECRAB_NET_HELPER_SOCK:-/run/firecrab/net-helper.sock}
DNSMASQ_CONF=${FIRECRAB_DNSMASQ_CONF:-/run/firecrab/dnsmasq.conf}
DNSMASQ_PID=${FIRECRAB_DNSMASQ_PID:-/run/firecrab/dnsmasq.pid}

OK=0
FAIL=0
SKIP=0
# Accumulated problem lines (FAIL/SKIP only) for a quiet pass summary.
REPORT=()

# The individual checks only *accumulate* into REPORT/OK/FAIL/SKIP; nothing
# is printed until the very end (see "run" below), so a bug in any one check
# that trips `set -euo pipefail` (an unquoted expansion, a pipeline member
# failing, an unbound var) aborts the whole script with zero visible output
# — a totally silent failure that gives no clue which check misbehaved. This
# trap guarantees a diagnosable trace even for a crash the checks
# themselves never anticipated.
# Invoked indirectly via the ERR trap below; shellcheck can't see that, and
# different versions flag it differently (SC2329 on the signature, or
# SC2317 "unreachable" on every line in the body) — disable both.
# shellcheck disable=SC2329,SC2317
on_unexpected_error() {
    local line=$1 command=$2
    printf 'doctor: internal error at line %s: %s\n' "$line" "$command" >&2
    printf '(this is a bug in firecrab-doctor.sh itself, not a host problem)\n' >&2
    if [ "${#REPORT[@]}" -gt 0 ]; then
        printf 'partial results before the crash:\n' >&2
        printf '%s\n' "${REPORT[@]}" >&2
    fi
}
trap 'on_unexpected_error "$LINENO" "$BASH_COMMAND"' ERR

usage() {
    cat <<'USAGE'
Usage: firecrab-doctor [--digest] [-h|--help]

  Diagnose host readiness for firecrab. Changes nothing; root not required.
  Privileged checks that cannot run are listed as SKIP with a fix hint.

  --digest   also print sha256 (first 12 hex chars) of template images
  -h, --help this text

Environment: DATADIR, FIRECRAB_NET_HELPER_SOCK, FIRECRAB_IMAGE_ROOT,
             FIRECRAB_DNSMASQ_CONF, FIRECRAB_DNSMASQ_PID
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --digest) DIGEST=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

have() { command -v "$1" >/dev/null 2>&1; }

pass() {
    OK=$((OK + 1))
}

# Append each non-empty line of $1 as an indented report line.
report_detail() {
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        REPORT+=("  ${line}")
    done <<<"$1"
}

# fail "short title" "detail" "fix one-liner"
fail() {
    FAIL=$((FAIL + 1))
    local title=$1 detail=${2:-} fix=${3:-}
    REPORT+=("[FAIL] ${title}")
    if [ -n "$detail" ]; then
        report_detail "$detail"
    fi
    if [ -n "$fix" ]; then
        REPORT+=("  → ${fix}")
    fi
}

skip() {
    SKIP=$((SKIP + 1))
    local title=$1 detail=${2:-} fix=${3:-}
    REPORT+=("[SKIP] ${title}")
    if [ -n "$detail" ]; then
        report_detail "$detail"
    fi
    if [ -n "$fix" ]; then
        REPORT+=("  → ${fix}")
    fi
}

# --- individual checks -------------------------------------------------------

check_kvm() {
    if [ ! -e /dev/kvm ]; then
        fail "kvm: /dev/kvm missing" \
            "VMs cannot start without KVM" \
            "enable virtualization in BIOS, or nested virt if this host is itself a VM"
        return
    fi
    if [ ! -c /dev/kvm ]; then
        fail "kvm: /dev/kvm is not a character device"
        return
    fi
    if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
        pass
        return
    fi
    fail "kvm: current user cannot read/write /dev/kvm" \
        "user=$(id -un) groups=$(id -nG 2>/dev/null || true)" \
        "sudo usermod -aG kvm \"$(id -un)\"; then log out and back in"
}

check_firecracker() {
    local bin ver
    if [ -n "${FIRECRAB_FIRECRACKER_BIN:-}" ]; then
        bin=$FIRECRAB_FIRECRACKER_BIN
    elif have firecracker; then
        bin=$(command -v firecracker)
    else
        fail "firecracker: binary not found on PATH" \
            "" \
            "run scripts/install-firecracker.sh or sudo ./install.sh"
        return
    fi
    if [ ! -x "$bin" ]; then
        fail "firecracker: $bin is not executable" \
            "" \
            "run scripts/install-firecracker.sh"
        return
    fi
    # Firecracker prints a banner and exits 0 on --version.
    ver=$("$bin" --version 2>&1 | head -n1 | tr -d '\r' || true)
    if [ -z "$ver" ]; then
        fail "firecracker: could not read version from $bin"
        return
    fi
    pass
}

check_ip_forward() {
    local val
    if [ ! -r /proc/sys/net/ipv4/ip_forward ]; then
        skip "ip_forward: /proc/sys/net/ipv4/ip_forward unreadable" \
            "" \
            "cat /proc/sys/net/ipv4/ip_forward"
        return
    fi
    val=$(tr -d ' \n' </proc/sys/net/ipv4/ip_forward)
    if [ "$val" = "1" ]; then
        pass
        return
    fi
    fail "ip_forward: net.ipv4.ip_forward is $val (want 1)" \
        "guest outbound NAT needs forwarding" \
        "sudo sysctl -w net.ipv4.ip_forward=1  (net-helper also sets this on start)"
}

check_nft() {
    if ! have nft; then
        fail "nft: nftables binary not found" \
            "" \
            "install nftables (apt/dnf/… package: nftables)"
        return
    fi

    local out rc=0
    # `nft list tables` needs CAP_NET_ADMIN on many kernels.
    out=$(nft list tables 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        case "$out" in
            *"Operation not permitted"*|*"Permission denied"*|*"must be root"*)
                skip "nft: cannot list tables (permission denied)" \
                    "cannot confirm inet firecrab / bridge firecrab_l2" \
                    "re-run as root: sudo nft list tables"
                return
                ;;
            *)
                fail "nft: list tables failed" "$out" "sudo nft list tables"
                return
                ;;
        esac
    fi

    local missing=()
    printf '%s\n' "$out" | grep -Eq '(^|[[:space:]])table inet firecrab($|[[:space:]])' \
        || missing+=("inet firecrab")
    printf '%s\n' "$out" | grep -Eq '(^|[[:space:]])table bridge firecrab_l2($|[[:space:]])' \
        || missing+=("bridge firecrab_l2")

    if [ "${#missing[@]}" -eq 0 ]; then
        pass
        return
    fi

    # Tables appear after API/helper has run ensure_firewall at least once
    # (daemon start or first MicroNetwork). Helper socket alone is not enough
    # if the API never connected yet — skip rather than hard-fail that race.
    if [ -S "$HELPER_SOCK" ]; then
        # Zero bridges and no tables: install-fresh with no MicroNetwork yet.
        # Ruleset may still be applied empty; if tables are missing entirely,
        # treat as soft until a network or VM start forces ensure.
        if [ -z "$(list_firecrab_bridges)" ]; then
            skip "nft: firecrab tables not present yet (${missing[*]})" \
                "ok with zero MicroNetworks until ensure_firewall runs" \
                "POST /api/micro-networks or restart firecrab-api"
            return
        fi
        fail "nft: missing firecrab tables: ${missing[*]}" \
            "net-helper socket is up but tables are absent" \
            "systemctl restart firecrab-net-helper  (or start a VM so rules are applied)"
    else
        skip "nft: firecrab tables not present yet (${missing[*]})" \
            "expected after firecrab-net-helper is running" \
            "start firecrab-net-helper, then re-run doctor"
    fi
}

check_dnsmasq() {
    local pid alive=0 interfaces="" conf_ifaces=""

    if [ -f "$DNSMASQ_PID" ]; then
        pid=$(tr -d ' \n' <"$DNSMASQ_PID" 2>/dev/null || true)
        if [ -n "$pid" ] && [ -d "/proc/$pid" ]; then
            # Confirm it still looks like dnsmasq (pid reuse guard).
            if tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q dnsmasq; then
                alive=1
            fi
        fi
    fi

    if [ "$alive" -eq 0 ]; then
        # Fallback: a firecrab conf-file process.
        if have pgrep && pgrep -af 'dnsmasq.*firecrab' >/dev/null 2>&1; then
            alive=1
        fi
    fi

    if [ -r "$DNSMASQ_CONF" ]; then
        conf_ifaces=$(grep -E '^interface=' "$DNSMASQ_CONF" 2>/dev/null \
            | sed 's/^interface=//' | tr '\n' ' ' | sed 's/[[:space:]]*$//')
    fi

    # Collect firecrab bridges currently on the host (mnb* only after
    # explicit MicroNetwork create; no implicit fcbr0).
    interfaces=$(list_firecrab_bridges | tr '\n' ' ' | sed 's/[[:space:]]*$//')

    if [ "$alive" -eq 0 ]; then
        if [ -S "$HELPER_SOCK" ]; then
            # Zero MicroNetworks: helper is healthy but has no interface to
            # serve — dnsmasq is expected idle until the first network exists.
            if [ -z "$interfaces" ]; then
                pass
                return
            fi
            fail "dnsmasq: no firecrab dnsmasq process" \
                "pid_file=$DNSMASQ_PID conf=$DNSMASQ_CONF bridges: $interfaces" \
                "systemctl restart firecrab-net-helper  (or create a MicroNetwork)"
        else
            skip "dnsmasq: not running (helper also down)" \
                "" \
                "start firecrab-net-helper"
        fi
        return
    fi

    if [ -n "$interfaces" ] && [ -n "$conf_ifaces" ]; then
        local br missing_if=()
        for br in $interfaces; do
            case " $conf_ifaces " in
                *" $br "*) ;;
                *) missing_if+=("$br") ;;
            esac
        done
        if [ "${#missing_if[@]}" -gt 0 ]; then
            fail "dnsmasq: conf missing interface(s): ${missing_if[*]}" \
                "serving: $conf_ifaces  bridges: $interfaces" \
                "systemctl restart firecrab-net-helper  (rewrites $DNSMASQ_CONF)"
            return
        fi
    fi

    pass
}

check_helper_socket() {
    local path mode owner group

    if [ ! -e "$HELPER_SOCK" ]; then
        fail "helper socket: $HELPER_SOCK does not exist" \
            "API cannot reach the network helper" \
            "systemctl start firecrab-net-helper  (dev: ./scripts/dev-net-helper.sh)"
        return
    fi

    if [ ! -S "$HELPER_SOCK" ]; then
        fail "helper socket: $HELPER_SOCK exists but is not a socket"
        return
    fi

    if have stat; then
        mode=$(stat -c '%a' "$HELPER_SOCK" 2>/dev/null || true)
        owner=$(stat -c '%U' "$HELPER_SOCK" 2>/dev/null || true)
        group=$(stat -c '%G' "$HELPER_SOCK" 2>/dev/null || true)
    else
        mode=; owner=; group=
    fi

    # Access: the API process must be able to connect. Test as current user.
    if [ ! -r "$HELPER_SOCK" ] || [ ! -w "$HELPER_SOCK" ]; then
        fail "helper socket: not accessible by current user" \
            "path=$HELPER_SOCK mode=${mode:-?} owner=${owner:-?}:${group:-?}" \
            "unit Group= must match the API account group (socket is 0660); add user to that group"
        return
    fi

    # Preferred mode is 660 (group-shared). 600/620 block the non-owner API user.
    if [ "$mode" = "600" ] || [ "$mode" = "620" ]; then
        fail "helper socket: mode $mode is too tight for a group-shared socket" \
            "path=$HELPER_SOCK owner=${owner:-?}:${group:-?}" \
            "ensure firecrab-net-helper runs with Group= shared with the API (chmod 0660)"
        return
    fi

    pass
}

# Prints firecrab-owned bridge names, one per line (mnb*; legacy fcbr0 if any).
list_firecrab_bridges() {
    if ! have ip; then
        return 0
    fi
    # `ip -br link show type bridge` → first field is the name.
    ip -br link show type bridge 2>/dev/null \
        | awk '$1 == "fcbr0" || $1 ~ /^mnb/ { print $1 }'
}

# Default IPv4 uplink interface name, or empty.
detect_uplink() {
    if ! have ip; then
        return 0
    fi
    ip -4 route get 8.8.8.8 2>/dev/null \
        | awk '{ for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit } }'
}

# Returns 0 if UFW looks enabled (best-effort without root).
ufw_is_enabled() {
    if [ -r /etc/ufw/ufw.conf ]; then
        grep -Eq '^[[:space:]]*ENABLED=yes[[:space:]]*$' /etc/ufw/ufw.conf 2>/dev/null \
            && return 0
        # Explicitly disabled.
        grep -Eq '^[[:space:]]*ENABLED=no[[:space:]]*$' /etc/ufw/ufw.conf 2>/dev/null \
            && return 1
    fi
    # Fall through to the CLI if present.
    local status
    if status=$(ufw status 2>/dev/null); then
        case "$status" in
            *"Status: active"*|*"상태: 활성"*) return 0 ;;
            *) return 1 ;;
        esac
    fi
    return 1
}

# Best-effort dump of ufw status verbose (needs root on most hosts).
ufw_status_verbose() {
    ufw status verbose 2>/dev/null
}

# $1 = bridge name, $2 = full status text
# Returns 0 if INPUT allow for 67/udp on that bridge is present.
ufw_has_dhcp_allow() {
    local br=$1 status=$2
    # English/locale-agnostic: action column stays "ALLOW IN"; rule form is
    # "67/udp on <iface>" (IPv6 lines are ignored for this check).
    printf '%s\n' "$status" | grep -Eiq "^67/udp on ${br}[[:space:]]+ALLOW IN" \
        || printf '%s\n' "$status" | grep -Eiq "^67 on ${br}[[:space:]]+ALLOW IN"
}

# $1 = bridge, status text: DNS 53 tcp or udp on bridge.
ufw_has_dns_allow() {
    local br=$1 status=$2
    printf '%s\n' "$status" | grep -Eiq "^53/(udp|tcp) on ${br}[[:space:]]+ALLOW IN" \
        || printf '%s\n' "$status" | grep -Eiq "^53 on ${br}[[:space:]]+ALLOW IN"
}

# $1 = bridge, $2 = uplink, $3 = status: route allow in on br out on uplink.
# ufw status shows route rules as:
#   Anywhere on <out-if>    ALLOW FWD   Anywhere on <in-if>
ufw_has_route_allow() {
    local br=$1 uplink=$2 status=$3
    printf '%s\n' "$status" \
        | grep -Eiq "on ${uplink}[[:space:]]+ALLOW FWD[[:space:]].*on ${br}([[:space:]]|$)"
}

check_ufw() {
    if ! have ufw && [ ! -e /etc/ufw/ufw.conf ]; then
        pass  # UFW not installed — nothing to diagnose.
        return
    fi

    if ! ufw_is_enabled; then
        pass  # inactive: firecrab's own nft tables are enough.
        return
    fi

    local status bridges uplink br
    status=$(ufw_status_verbose || true)
    if [ -z "$status" ]; then
        skip "ufw: active but status is not readable without root" \
            "UFW commonly blocks DHCP on new bridges and VM outbound forwards" \
            "sudo ufw status verbose   # then allow 67/udp+53 on each fcbr0/mnb* bridge and: sudo ufw route allow in on <bridge> out on <uplink>"
        return
    fi

    bridges=$(list_firecrab_bridges)
    if [ -z "$bridges" ]; then
        # No bridges yet — cannot assert per-bridge rules. Warn that UFW is
        # active so the operator knows to re-check after the first network.
        skip "ufw: active, but no firecrab bridges (fcbr0/mnb*) yet" \
            "new bridges need DHCP/DNS and route allows (docs/20-guides/troubleshooting.md)" \
            "after net-helper starts: sudo ufw allow in on fcbr0 to any port 67 proto udp; …"
        return
    fi

    uplink=$(detect_uplink)
    local any_fail=0

    while IFS= read -r br; do
        [ -n "$br" ] || continue
        if ! ufw_has_dhcp_allow "$br" "$status"; then
            fail "ufw: bridge $br missing allow 67/udp (DHCP)" \
                "guest will fail with no-ipv4-address" \
                "sudo ufw allow in on $br to any port 67 proto udp"
            any_fail=1
        fi
        if ! ufw_has_dns_allow "$br" "$status"; then
            fail "ufw: bridge $br missing allow 53 (DNS)" \
                "guest may fail with dns-unreachable" \
                "sudo ufw allow in on $br to any port 53"
            any_fail=1
        fi
        if [ -n "$uplink" ]; then
            if ! ufw_has_route_allow "$br" "$uplink" "$status"; then
                fail "ufw: no route allow $br → $uplink" \
                    "guest outbound new connections time out (DEFAULT_FORWARD_POLICY=DROP)" \
                    "sudo ufw route allow in on $br out on $uplink"
                any_fail=1
            fi
        fi
    done <<<"$bridges"

    if [ -z "$uplink" ]; then
        skip "ufw: could not detect uplink interface" \
            "cannot verify route allow rules" \
            "ip route get 8.8.8.8  # then: sudo ufw route allow in on <bridge> out on <uplink>"
    fi

    # DHCP/DNS (and route, when uplink is known) all present.
    if [ "$any_fail" -eq 0 ]; then
        pass
    fi
}

# Collect candidate firecrab.db absolute paths (existing files only).
find_databases() {
    local candidates=() c
    candidates+=("$PWD/data/firecrab.db")
    candidates+=("$DATADIR/data/firecrab.db")
    # WorkingDirectory=@DATADIR@ means the relative path is data/firecrab.db
    # under DATADIR; some layouts also put the file directly in DATADIR.
    candidates+=("$DATADIR/firecrab.db")
    if [ -n "${FIRECRAB_IMAGE_ROOT:-}" ]; then
        # Image root is usually $DATADIR/images; DB lives next to images' parent.
        candidates+=("$(cd -- "$(dirname -- "$FIRECRAB_IMAGE_ROOT")" 2>/dev/null && pwd)/data/firecrab.db")
    fi

    local seen="" path
    for c in "${candidates[@]}"; do
        [ -f "$c" ] || continue
        path=$(cd -- "$(dirname -- "$c")" && pwd)/$(basename -- "$c")
        case " $seen " in
            *" $path "*) continue ;;
        esac
        seen+=" $path"
        printf '%s\n' "$path"
    done
}

check_data_root() {
    local dbs free_line root
    mapfile -t dbs < <(find_databases)

    if [ "${#dbs[@]}" -gt 1 ]; then
        local listing
        listing=$(printf '%s\n' "${dbs[@]}")
        fail "data: multiple firecrab.db files found" \
            "$listing" \
            "API resolves data/firecrab.db relative to cwd — use the unit WorkingDirectory ($DATADIR) or always start from the same directory (avoids no-such-column / empty state)"
        return
    fi

    if [ "${#dbs[@]}" -eq 1 ]; then
        root=$(dirname -- "$(dirname -- "${dbs[0]}")")
        # Prefer the directory that actually holds data/.
        if [ -d "$root/data" ]; then
            :
        else
            root=$(dirname -- "${dbs[0]}")
        fi
        if have df; then
            free_line=$(df -h "$root" 2>/dev/null | awk 'NR==2 { print $4 " free on " $6 }' || true)
            if [ -n "$free_line" ]; then
                # Free space is informational when ok; still pass.
                :
            fi
        fi
        pass
        return
    fi

    # No DB yet is fine on a fresh host; warn if neither cwd nor DATADIR is usable.
    if [ -d "$DATADIR" ] || [ -d "$PWD/data" ]; then
        pass
        return
    fi

    skip "data: no firecrab.db and no data directory yet" \
        "looked at $PWD/data and $DATADIR" \
        "sudo ./install.sh   # or mkdir -p data when developing from the repo root"
}

# Template artifacts expected by firecrab-api/src/templates.rs default_specs().
# Paths are relative to an image root.
template_artifacts() {
    cat <<'EOF'
kernel/vmlinux-ubuntu-26.04-x86_64
rootfs/ubuntu-rootfs-26.04-amd64.ext4
kernel/vmlinux-alpine-virt-x86_64
kernel/initramfs-alpine-virt-x86_64
rootfs/alpine-rootfs-3.24.1-x86_64.ext4
EOF
}

resolve_image_roots() {
    local roots=() r
    if [ -n "${FIRECRAB_IMAGE_ROOT:-}" ]; then
        roots+=("$FIRECRAB_IMAGE_ROOT")
    fi
    roots+=("$PWD/images")
    roots+=("$DATADIR/images")

    local seen="" path
    for r in "${roots[@]}"; do
        [ -d "$r" ] || continue
        path=$(cd -- "$r" && pwd)
        case " $seen " in
            *" $path "*) continue ;;
        esac
        seen+=" $path"
        printf '%s\n' "$path"
    done
}

short_digest() {
    local file=$1
    if have sha256sum; then
        sha256sum -- "$file" 2>/dev/null | awk '{ print substr($1, 1, 12) }'
    elif have shasum; then
        shasum -a 256 -- "$file" 2>/dev/null | awk '{ print substr($1, 1, 12) }'
    else
        printf 'unavailable'
    fi
}

check_images() {
    local roots
    mapfile -t roots < <(resolve_image_roots)

    if [ "${#roots[@]}" -eq 0 ]; then
        fail "images: no image root found" \
            "looked at FIRECRAB_IMAGE_ROOT, $PWD/images, $DATADIR/images" \
            "sudo ./install.sh   # or build with scripts/firecracker-menual/install-alpine-rootfs.sh"
        return
    fi

    # Prefer the first root that has any of the expected artifacts.
    local root="" candidate art
    for candidate in "${roots[@]}"; do
        while IFS= read -r art; do
            if [ -f "$candidate/$art" ]; then
                root=$candidate
                break 2
            fi
        done < <(template_artifacts)
    done

    if [ -z "$root" ]; then
        # Any ext4 under rootfs/ counts as "something is there" but incomplete.
        local any=0
        for candidate in "${roots[@]}"; do
            if compgen -G "$candidate/rootfs/*.ext4" >/dev/null 2>&1; then
                any=1
                root=$candidate
                break
            fi
        done
        if [ "$any" -eq 0 ]; then
            fail "images: no guest rootfs found under ${roots[*]}" \
                "" \
                "sudo ./install.sh  or copy images into $DATADIR/images"
            return
        fi
    fi

    local missing=() present=0
    while IFS= read -r art; do
        if [ -f "$root/$art" ]; then
            present=$((present + 1))
            if [ "$DIGEST" -eq 1 ]; then
                # Digests are only printed on failure paths normally; with
                # --digest we attach them as detail on the final fail/pass.
                # Store nothing here — counted via present.
                :
            fi
        else
            missing+=("$art")
        fi
    done < <(template_artifacts)

    if [ "$present" -eq 0 ]; then
        fail "images: image root $root has none of the default template artifacts" \
            "" \
            "build or copy templates into $root"
        return
    fi

    # Missing some templates is a soft fail: the API starts with a subset.
    if [ "${#missing[@]}" -gt 0 ]; then
        # Not a hard fail — API skips missing templates. Report as skip so
        # quiet mode still surfaces it without blocking "all checks passed".
        skip "images: some default templates missing under $root" \
            "missing: ${missing[*]}" \
            "build the missing image(s), copy into $root, restart firecrab-api"
    else
        if [ "$DIGEST" -eq 1 ]; then
            local digests=()
            while IFS= read -r art; do
                digests+=("$art=$(short_digest "$root/$art")")
            done < <(template_artifacts)
            # Still a pass; digests only on request go to stderr so the quiet
            # pass summary stays one line, while --digest users see hashes.
            printf 'images: %s\n' "${digests[*]}" >&2
        fi
        pass
    fi
}

# --- run ---------------------------------------------------------------------

check_kvm
check_firecracker
check_ip_forward
check_nft
check_dnsmasq
check_helper_socket
check_ufw
check_data_root
check_images

if [ "$FAIL" -eq 0 ] && [ "$SKIP" -eq 0 ]; then
    printf 'doctor: all checks passed (%s ok)\n' "$OK"
    exit 0
fi

if [ "$FAIL" -eq 0 ]; then
    printf 'doctor: %s ok, %s skipped (no failures)\n' "$OK" "$SKIP"
else
    printf 'doctor: %s failed, %s skipped, %s ok\n' "$FAIL" "$SKIP" "$OK"
fi

local_line=
for local_line in "${REPORT[@]}"; do
    printf '%s\n' "$local_line"
done

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
