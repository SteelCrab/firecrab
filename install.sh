#!/usr/bin/env bash
# firecrab host installer (public-docs/installation.md).
#
# Downloads a host bundle from a GitHub Release (or installs local binaries
# via --bin-dir), then lays out systemd units so the dashboard is served on
# http://127.0.0.1:3000/.
#
# Bundles are Linux x86_64/aarch64 × gnu (glibc) / musl. glibc hosts get the
# gnu bundle; musl hosts (Alpine) get musl. Override with --libc.
#
# Every step checks before it changes anything, so re-running is safe and
# doubles as a repair for a half-broken install.
set -euo pipefail

FIRECRAB_USER=${FIRECRAB_USER:-firecrab}
FIRECRAB_GROUP=${FIRECRAB_GROUP:-firecrab}
PREFIX=${PREFIX:-/usr/local}
LIBDIR="$PREFIX/lib/firecrab"
SHAREDIR="$PREFIX/share/firecrab"
DATADIR=${DATADIR:-/var/lib/firecrab}
CONFDIR=${CONFDIR:-/etc/firecrab}
UNITDIR=${UNITDIR:-/etc/systemd/system}

# stdin (`curl | bash`) leaves BASH_SOURCE unset under `set -u`.
# `bash <(curl …)` sets it to a /dev/fd path that is not the checkout.
_src=${BASH_SOURCE[0]:-${0:-}}
case "$_src" in
    ""|-|bash|sh|main|/dev/fd/*|/proc/self/fd/*)
        REPO_ROOT=$(pwd)
        ;;
    *)
        REPO_ROOT=$(cd -- "$(dirname -- "$_src")" && pwd)
        ;;
esac
unset _src
UNITS=(firecrab-net-helper.service firecrab-api.service)

# Placeholder @FIRECRAB_RELEASE_TAG@ is replaced by the tag on a published
# install.sh (v0.1.0). The repo copy keeps the placeholder and means "latest".
FIRECRAB_RELEASE_TAG="@FIRECRAB_RELEASE_TAG@"

MODE=install
INSTALL_DEPS=1
WITH_IMAGES=0
WITH_UBUNTU_IMAGE=0
WITH_ROCKY_IMAGE=0
WITH_FRONTEND=1
PURGE=0
BIN_DIR=
DASHBOARD_DIR=
RELEASE_VERSION=
LIBC=
PAYLOAD_TMP=
PAYLOAD_ROOT=
PAYLOAD_BIN=
PAYLOAD_UNITS=
PAYLOAD_EXTRACT=
PAYLOAD_DOCTOR=
PAYLOAD_DASHBOARD=

# BEGIN RELEASE_HELPERS
if [ -f "$REPO_ROOT/scripts/firecrab-release.sh" ]; then
    # Optional checkout helper; the published installer inlines this block.
    # shellcheck disable=SC1091
    . "$REPO_ROOT/scripts/firecrab-release.sh"
fi
if ! type firecrab_host_arch >/dev/null 2>&1; then
    printf 'xx  release helpers missing — run from a firecrab checkout or use the install.sh from a GitHub Release\n' >&2
    exit 1
fi
# END RELEASE_HELPERS

PKG=''          # detected package manager
PKG_UPDATED=0   # index refreshed at most once

# Help text, also printed before failing on an unknown option.
usage() {
    cat <<'USAGE'
Usage: ./install.sh [OPTIONS]

  (no options)        download the latest release binaries and start firecrab
  --check             report what is missing and what would be installed
  --doctor            run host diagnostics (UFW, socket, KVM, nft, …); no root required
  --deps-only         install the dependencies, then stop (no host changes)
  --no-deps           never install packages, only report gaps
  --no-images         skip guest images (default)
  --with-images       also install Alpine from MicroRegistry
  --with-ubuntu-image also install Ubuntu 26.04 (large)
  --with-rocky-image  also install Rocky Linux 9.8 (large)
  --no-frontend       do not install the dashboard
  --version VER       GitHub Release tag (default: latest, or the baked-in tag)
  --libc gnu|musl     host libc (default: detect; gnu = glibc)
  --bin-dir DIR       install these local binaries instead of the release
  --dashboard-dir DIR install this built dashboard (with --bin-dir)
  --print-install-url print the curl install command and exit
  --print-release-url print the host-bundle URL and exit
  --uninstall         stop and remove units, binaries and dashboard
  --purge             with --uninstall, also delete /var/lib/firecrab (VM data!)
  -h, --help          this text

  curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash

  Host bundles (install.sh picks arch + libc):
    firecrab-host-x86_64-gnu.tar.gz    Linux x86_64, glibc
    firecrab-host-x86_64-musl.tar.gz   Linux x86_64, musl (static)
    firecrab-host-aarch64-gnu.tar.gz   Linux aarch64, glibc
    firecrab-host-aarch64-musl.tar.gz  Linux aarch64, musl (static)

Environment: FIRECRAB_USER, FIRECRAB_GROUP, PREFIX, DATADIR, CONFDIR, UNITDIR
             FIRECRAB_RELEASE_REPO, FIRECRAB_DEFAULT_VERSION
USAGE
}

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
step() { printf '\033[1;36m--\033[0m  %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# Privilege escalation: empty when already root, "sudo" otherwise.
# Individual commands that need root use $SUDO; the whole script runs as the
# invoking user so that $HOME, SSH keys, and cargo are the user's, not root's.
if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
else
    have sudo || die "sudo is required to run privileged steps; install it first"
    SUDO="sudo"
fi

# Extra args after --doctor are forwarded to scripts/firecrab-doctor.sh.
DOCTOR_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --check) MODE=check ;;
        --doctor)
            MODE=doctor
            shift
            # Everything after --doctor belongs to the doctor script.
            DOCTOR_ARGS=("$@")
            break
            ;;
        --deps-only) MODE=deps ;;
        --no-deps) INSTALL_DEPS=0 ;;
        --no-images) WITH_IMAGES=0 ;;
        --with-images) WITH_IMAGES=1 ;;
        --with-ubuntu-image) WITH_UBUNTU_IMAGE=1 ;;
        --with-rocky-image) WITH_ROCKY_IMAGE=1 ;;
        --no-frontend) WITH_FRONTEND=0 ;;
        --version)
            [ $# -ge 2 ] || die "--version needs a tag (for example v0.1.0)"
            RELEASE_VERSION=$2
            shift
            ;;
        --libc)
            [ $# -ge 2 ] || die "--libc needs gnu or musl"
            LIBC=$2
            firecrab_normalize_libc "$LIBC" >/dev/null \
                || die "--libc must be gnu, glibc, or musl"
            LIBC=$(firecrab_normalize_libc "$LIBC")
            shift
            ;;
        --bin-dir)
            [ $# -ge 2 ] || die "--bin-dir needs a directory"
            BIN_DIR=$2
            shift
            ;;
        --dashboard-dir)
            [ $# -ge 2 ] || die "--dashboard-dir needs a directory"
            DASHBOARD_DIR=$2
            shift
            ;;
        --print-install-url) MODE=print-install-url ;;
        --print-release-url) MODE=print-release-url ;;
        --uninstall) MODE=uninstall ;;
        --purge) PURGE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
    shift
done

if [ -z "$RELEASE_VERSION" ]; then
    RELEASE_VERSION=${FIRECRAB_DEFAULT_VERSION:-}
fi
# A published installer substitutes the placeholder with the tag. The repo
# copy keeps @FIRECRAB_RELEASE_TAG@, which means "latest".
case "$FIRECRAB_RELEASE_TAG" in
    ""|@*) ;;
    *)
        if [ -z "$RELEASE_VERSION" ]; then
            RELEASE_VERSION=$FIRECRAB_RELEASE_TAG
        fi
        ;;
esac

if [ -z "$LIBC" ]; then
    LIBC=$(firecrab_host_libc)
else
    LIBC=$(firecrab_normalize_libc "$LIBC") || die "--libc must be gnu, glibc, or musl"
fi

cleanup_payload() {
    if [ -n "${PAYLOAD_TMP:-}" ] && [ -d "$PAYLOAD_TMP" ]; then
        rm -rf "$PAYLOAD_TMP"
    fi
}
trap cleanup_payload EXIT

# --- package manager -------------------------------------------------------

# Sets PKG to the first package manager found; returns 1 if none is.
detect_pkg() {
    for candidate in apt-get dnf zypper pacman apk; do
        if have "$candidate"; then PKG=$candidate; return 0; fi
    done
    return 1
}

# Package name per manager, since only a few of these agree.
pkg_name() {
    local generic=$1
    case "$PKG:$generic" in
        # Fedora/RHEL call it iproute; everyone else iproute2.
        dnf:iproute2) echo "iproute" ;;
        *:iproute2)   echo "iproute2" ;;
        *:nftables)   echo "nftables" ;;
        *:dnsmasq)    echo "dnsmasq" ;;
        # dhcp_release is shipped separately on Debian/Ubuntu and Alpine.
        # On RHEL/Fedora it does not exist as a separate package.
        apt-get:dnsmasq-utils) echo "dnsmasq-utils" ;;
        apk:dnsmasq-utils)     echo "dnsmasq-utils" ;;
        dnf:dnsmasq-utils)     echo "dnsmasq" ;;
        *:dnsmasq-utils)       echo "dnsmasq-utils" ;;
        apk:e2fsprogs) echo "e2fsprogs-extra" ;;
        *:e2fsprogs)  echo "e2fsprogs" ;;
        *:curl)       echo "curl" ;;
        # scripts/install-firecracker.sh needs find and tar; a minimal
        # openSUSE or Debian install has neither.
        *:findutils)  echo "findutils" ;;
        *:tar)        echo "tar" ;;
        apt-get:xz)   echo "xz-utils" ;;
        *:xz)         echo "xz" ;;
        # Dashboard M2Image install decompresses `{alias}.tar.zst` packages.
        *:zstd)       echo "zstd" ;;
        *) echo "$generic" ;;
    esac
}

# Refreshes the package index at most once per run.
pkg_refresh() {
    [ "$PKG_UPDATED" -eq 1 ] && return 0
    case "$PKG" in
        apt-get) DEBIAN_FRONTEND=noninteractive $SUDO apt-get update -qq ;;
        apk) $SUDO apk update -q ;;
        *) : ;;  # dnf/zypper/pacman refresh as part of install
    esac
    PKG_UPDATED=1
}

# Installs the named packages with whichever manager was detected.
# One retry: Fedora metalink/mirror writes fail transiently.
pkg_install() {
    pkg_refresh
    local attempt
    for attempt in 1 2; do
        if pkg_install_once "$@"; then
            return 0
        fi
        [ "$attempt" -eq 1 ] || return 1
        warn "package install failed; retrying once"
        sleep 2
    done
}

pkg_install_once() {
    case "$PKG" in
        apt-get) DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "$@" ;;
        dnf) $SUDO dnf install -y -q "$@" ;;
        zypper) $SUDO zypper --non-interactive install -y "$@" ;;
        pacman) $SUDO pacman -Sy --noconfirm --needed "$@" ;;
        apk) $SUDO apk add --no-cache "$@" ;;
        *) return 1 ;;
    esac
}

# `ensure <command> <generic package>`: install only what is actually absent.
ensure() {
    local command_name=$1 generic=$2 packages
    have "$command_name" && return 0

    # One generic name can map to two packages (nodejs + npm), so the words are
    # split here, deliberately, into an array rather than left to the shell.
    read -ra packages <<<"$(pkg_name "$generic")"
    if [ "$MODE" = check ]; then
        warn "missing: $command_name (would install: ${packages[*]})"
        return 1
    fi
    [ "$INSTALL_DEPS" -eq 1 ] || { warn "missing: $command_name (--no-deps, install '${packages[*]}' yourself)"; return 1; }
    [ -n "$PKG" ] || { warn "missing: $command_name and no known package manager — install '${packages[*]}' yourself"; return 1; }

    step "installing ${packages[*]} (for $command_name)"
    pkg_install "${packages[@]}" || { warn "failed to install ${packages[*]}"; return 1; }
    have "$command_name"
}

# --- host checks -----------------------------------------------------------

# A VM cannot start without /dev/kvm, and this script cannot supply it.
check_kvm() {
    if [ -e /dev/kvm ]; then
        log "/dev/kvm present"
        return 0
    fi
    # Not installable: it is the CPU, the BIOS, or a VM without nested virt.
    warn "/dev/kvm missing — enable virtualization in BIOS, or nested virt if this host is itself a VM"
    return 1
}

# The units need a running systemd, not merely the binary.
check_systemd() {
    if [ -d /run/systemd/system ]; then
        log "systemd is running"
        return 0
    fi
    warn "not a systemd host — the daemons can still be started by hand, but units will not work"
    return 1
}

# UFW is still reported here; the helper punches DHCP/DNS on each bridge
# for UFW, firewalld, iptables, and nftables.
report_ufw() {
    have ufw || return 0
    local status
    if status=$(ufw status 2>/dev/null); then
        case "$status" in
            *"Status: active"*)
                log "UFW is active — net-helper opens DHCP/DNS on each MicroNetwork bridge" ;;
            *) log "UFW installed but inactive" ;;
        esac
    else
        warn "UFW installed; re-run as root to see whether it is active"
    fi
}

# --- dependency resolution -------------------------------------------------

# The commands the two daemons shell out to while running, plus the handful the
# installer's own helper scripts need.
ensure_runtime_deps() {
    local failed=0
    ensure ip iproute2   || failed=1
    ensure nft nftables  || failed=1
    ensure dnsmasq dnsmasq || failed=1
    # dhcp_release releases DHCP leases on VM stop; VMs can still run without
    # it but leases accumulate until they expire (default 1 h). Fedora/RHEL
    # ship dnsmasq without that binary — do not reinstall dnsmasq for it.
    if ! have dhcp_release; then
        if [ "$PKG" = dnf ]; then
            warn "dhcp_release is not available on this distribution; leases expire on their own"
        else
            ensure dhcp_release dnsmasq-utils \
                || warn "dhcp_release not found; install dnsmasq-utils to release leases on VM stop"
        fi
    fi
    ensure mkfs.ext4 e2fsprogs || failed=1
    ensure curl curl     || failed=1
    ensure find findutils || failed=1
    ensure tar tar       || failed=1
    ensure zstd zstd     || failed=1
    ensure sha256sum coreutils || failed=1
    return $failed
}

# Comes from its own upstream script rather than a distro package.
# A piped release installer has no checkout, so fetch the same script.
ensure_firecracker() {
    have firecracker && { log "firecracker present"; return 0; }

    if [ "$MODE" = check ]; then
        warn "missing: firecracker (would install the upstream Firecracker binary)"
        return 1
    fi
    [ "$INSTALL_DEPS" -eq 1 ] || { warn "missing: firecracker (--no-deps; install it or re-run without --no-deps)"; return 1; }

    step "installing firecracker"
    local script fetched=
    script=$REPO_ROOT/scripts/install-firecracker.sh
    if [ ! -f "$script" ]; then
        fetched=$(mktemp)
        script=$fetched
        if ! curl --proto '=https' --tlsv1.2 -fsSL \
            "$(firecrab_repo_raw_url "${RELEASE_VERSION:-latest}" scripts/install-firecracker.sh)" \
            -o "$fetched"; then
            rm -f "$fetched"
            warn "firecracker install failed (could not download the installer)"
            return 1
        fi
    fi
    if $SUDO bash "$script"; then
        rm -f "$fetched"
        have firecracker && return 0
    fi
    rm -f "$fetched"
    warn "firecracker install failed"
    return 1
}

# Whether this script is running from a firecrab git checkout.
is_checkout() {
    [ -f "$REPO_ROOT/Cargo.toml" ] && [ -f "$REPO_ROOT/packaging/systemd/firecrab-api.service" ]
}

# Downloads the host bundle for this arch + libc.
download_host_bundle() {
    local arch tarball url sums_url
    arch=$(firecrab_host_arch) || die "unsupported architecture: $(uname -m) (need x86_64 or aarch64)"
    tarball=$(firecrab_host_tarball "$arch" "$LIBC")
    PAYLOAD_TMP=$(mktemp -d)
    url=$(firecrab_release_asset_url "$RELEASE_VERSION" "$tarball")
    sums_url=$(firecrab_release_asset_url "$RELEASE_VERSION" SHA256SUMS)
    step "downloading $tarball"
    if ! curl --proto '=https' --tlsv1.2 -fsSL "$url" -o "$PAYLOAD_TMP/$tarball"; then
        die "failed to download $url
Tag a GitHub Release, or install local binaries:
  cargo build --release -p firecrab-api -p firecrab-net-helper
  ./install.sh --bin-dir target/release --dashboard-dir firecrab-frontend/dist"
    fi
    if ! curl --proto '=https' --tlsv1.2 -fsSL "$sums_url" -o "$PAYLOAD_TMP/SHA256SUMS"; then
        die "failed to download $sums_url"
    fi
    firecrab_verify_sha256 "$PAYLOAD_TMP/SHA256SUMS" "$PAYLOAD_TMP/$tarball" \
        || die "checksum mismatch for $tarball"
    mkdir -p "$PAYLOAD_TMP/root"
    tar -xzf "$PAYLOAD_TMP/$tarball" -C "$PAYLOAD_TMP/root"
    PAYLOAD_ROOT=$PAYLOAD_TMP/root
    local name
    for name in firecrab-api firecrab-net-helper; do
        [ -x "$PAYLOAD_ROOT/$name" ] || die "release bundle is missing $name"
        firecrab_assert_binary_arch "$PAYLOAD_ROOT/$name" "$arch" \
            || die "$tarball: $name is not a $arch binary"
    done
    log "host bundle $arch/$LIBC"
}

# Sets PAYLOAD_* to either --bin-dir + this checkout, or a downloaded release.
resolve_payload() {
    if [ "$(firecrab_payload_mode "$BIN_DIR")" = dir ]; then
        [ -d "$BIN_DIR" ] || die "--bin-dir is not a directory: $BIN_DIR"
        is_checkout || die "--bin-dir needs a firecrab checkout for units and helper scripts"
        PAYLOAD_BIN=$BIN_DIR
        PAYLOAD_UNITS=$REPO_ROOT/packaging/systemd
        PAYLOAD_EXTRACT=$REPO_ROOT/scripts/firecracker-menual
        PAYLOAD_DOCTOR=$REPO_ROOT/scripts/firecrab-doctor.sh
        if [ -n "$DASHBOARD_DIR" ]; then
            PAYLOAD_DASHBOARD=$DASHBOARD_DIR
        elif [ -f "$REPO_ROOT/firecrab-frontend/dist/index.html" ]; then
            PAYLOAD_DASHBOARD=$REPO_ROOT/firecrab-frontend/dist
        else
            PAYLOAD_DASHBOARD=
        fi
        return 0
    fi

    download_host_bundle
    PAYLOAD_BIN=$PAYLOAD_ROOT
    PAYLOAD_UNITS=$PAYLOAD_ROOT/systemd
    PAYLOAD_EXTRACT=$PAYLOAD_ROOT
    PAYLOAD_DOCTOR=$PAYLOAD_ROOT/firecrab-doctor.sh
    PAYLOAD_DASHBOARD=$PAYLOAD_ROOT/dashboard
}

report_payload() {
    local tarball
    if [ "$(firecrab_payload_mode "$BIN_DIR")" = dir ]; then
        log "binaries: would install from $BIN_DIR"
        if [ ! -d "$BIN_DIR" ]; then
            warn "not a directory: $BIN_DIR"
            return 1
        fi
        local name gaps=0
        for name in firecrab-api firecrab-net-helper; do
            if [ -x "$BIN_DIR/$name" ]; then
                log "  $name"
            else
                warn "  missing $name (ok if already installed under $LIBDIR)"
                gaps=1
            fi
        done
        return 0
    fi
    if ! tarball=$(firecrab_host_tarball "" "$LIBC"); then
        warn "unsupported architecture: $(uname -m) (need x86_64 or aarch64)"
        return 1
    fi
    log "binaries: would download $(firecrab_release_asset_url "$RELEASE_VERSION" "$tarball") ($LIBC)"
    return 0
}

# --- host layout -----------------------------------------------------------

# The account the API runs as, in the kvm group for firecracker.
ensure_account() {
    if getent group "$FIRECRAB_GROUP" >/dev/null; then
        log "group $FIRECRAB_GROUP exists"
    else
        step "creating group $FIRECRAB_GROUP"
        $SUDO groupadd --system "$FIRECRAB_GROUP"
    fi

    if id "$FIRECRAB_USER" >/dev/null 2>&1; then
        log "user $FIRECRAB_USER exists"
    else
        step "creating user $FIRECRAB_USER"
        $SUDO useradd --system --gid "$FIRECRAB_GROUP" --home-dir "$DATADIR" \
                --shell /usr/sbin/nologin "$FIRECRAB_USER"
    fi

    # The API spawns firecracker, which opens /dev/kvm.
    if getent group kvm >/dev/null && ! id -nG "$FIRECRAB_USER" | tr ' ' '\n' | grep -qx kvm; then
        step "adding $FIRECRAB_USER to the kvm group"
        $SUDO usermod --append --groups kvm "$FIRECRAB_USER"
    fi

    # Group membership above is necessary but not always sufficient: some
    # hosts' /dev/kvm doesn't actually land in the kvm group the standard
    # udev rule (KERNEL=="kvm", GROUP="kvm") asks for — seen live on a
    # Parallels ARM host where it came up group "sgx" instead, which made
    # every VM start fail with "Kvm error: Permission denied (os error 13)"
    # even though $FIRECRAB_USER was correctly in the kvm group. An ACL
    # entry is a narrow, idempotent belt-and-suspenders fix: it grants the
    # kvm group access without touching whatever else owns the device node,
    # and re-running this is a no-op once the entry is already there.
    if [ -e /dev/kvm ]; then
        if have setfacl; then
            if ! getfacl -p /dev/kvm 2>/dev/null | grep -qx 'group:kvm:rw-'; then
                step "granting the kvm group ACL access to /dev/kvm"
                $SUDO setfacl -m g:kvm:rw /dev/kvm
            fi
        else
            warn "setfacl not found — skipping the /dev/kvm ACL fixup (install the 'acl' package if VMs fail with a KVM permission error)"
        fi
    fi
}

# Data, config and install directories with their intended owners.
ensure_directories() {
    $SUDO install -d -o root -g root -m 0755 "$LIBDIR" "$SHAREDIR"
    $SUDO install -d -o "$FIRECRAB_USER" -g "$FIRECRAB_GROUP" -m 0750 \
            "$DATADIR" "$DATADIR/data" "$DATADIR/images"
    $SUDO install -d -o root -g "$FIRECRAB_GROUP" -m 0750 "$CONFDIR"
    log "directories ready under $DATADIR, $CONFDIR"
}

# Puts the payload binaries, and the dashboard, where the units expect them.
install_binaries() {
    local name src
    for name in firecrab-api firecrab-net-helper; do
        src=$(firecrab_resolve_binary "$name" "$PAYLOAD_BIN" "$LIBDIR") \
            || die "no $name in ${PAYLOAD_BIN:-<release>} or $LIBDIR — pass it via --bin-dir or install a release"
        if [ "$src" -ef "$LIBDIR/$name" ]; then
            log "keeping existing $LIBDIR/$name"
        else
            $SUDO install -o root -g root -m 0755 "$src" "$LIBDIR/$name"
        fi
    done
    # extract-vmlinux and extract-arm64-image are shell scripts used at
    # runtime by the API to turn a distro vmlinuz into the kernel format
    # Firecracker boots on each architecture. Installing them next to the
    # binary lets the API find them without needing access to the repo
    # checkout (which may be in a user home the service account cannot reach).
    for helper in extract-vmlinux extract-arm64-image; do
        [ -f "$PAYLOAD_EXTRACT/$helper" ] || die "missing $PAYLOAD_EXTRACT/$helper"
        $SUDO install -o root -g root -m 0755 \
            "$PAYLOAD_EXTRACT/$helper" "$LIBDIR/$helper"
    done
    [ -f "$PAYLOAD_DOCTOR" ] || die "missing $PAYLOAD_DOCTOR"
    $SUDO install -d -o root -g root -m 0755 "$PREFIX/bin"
    $SUDO install -o root -g root -m 0755 "$PAYLOAD_DOCTOR" \
        "$PREFIX/bin/firecrab-doctor"
    log "binaries installed to $LIBDIR (doctor → $PREFIX/bin/firecrab-doctor)"

    [ "$WITH_FRONTEND" -eq 1 ] || return 0
    if [ -n "$PAYLOAD_DASHBOARD" ] && [ -f "$PAYLOAD_DASHBOARD/index.html" ]; then
        $SUDO rm -rf "$SHAREDIR/dashboard"
        $SUDO install -d -o root -g root -m 0755 "$SHAREDIR/dashboard"
        $SUDO cp -r "$PAYLOAD_DASHBOARD/." "$SHAREDIR/dashboard/"
        $SUDO chown -R root:root "$SHAREDIR/dashboard"
        log "dashboard installed to $SHAREDIR/dashboard"
    elif [ -f "$SHAREDIR/dashboard/index.html" ]; then
        log "keeping existing $SHAREDIR/dashboard"
    else
        die "dashboard not found — pass --dashboard-dir, or omit --no-frontend only when using a release bundle"
    fi
}

# On an enforcing SELinux host the services must not keep systemd's own
# domain.
#
# $PREFIX/lib/firecrab is labelled lib_t, and the policy rule that moves a
# service out of init_t into unconfined_service_t only fires for bin_t
# executables. Without this the API stays in init_t, where an outbound HTTPS
# connect is denied — every registry read fails with EACCES while a shell on
# the same account reaches the registry fine (public-docs/troubleshooting.md).
label_selinux_binaries() {
    local pattern="${LIBDIR}(/.*)?"

    have getenforce || return 0
    case "$(getenforce 2>/dev/null || printf Disabled)" in
        Enforcing|Permissive) ;;
        *) return 0 ;;
    esac

    if have semanage; then
        # -a fails when the rule already exists, which a re-run always hits.
        $SUDO semanage fcontext -a -t bin_t "$pattern" 2>/dev/null \
            || $SUDO semanage fcontext -m -t bin_t "$pattern" 2>/dev/null \
            || warn "could not record an SELinux file context for $LIBDIR"
    else
        warn "semanage not installed; the bin_t label will not survive a relabel" \
            "(dnf install policycoreutils-python-utils)"
    fi

    if have restorecon; then
        $SUDO restorecon -R "$LIBDIR" >/dev/null 2>&1 \
            || warn "restorecon failed for $LIBDIR"
        log "SELinux: $LIBDIR labelled bin_t"
    fi
}

# Seeds api.env once so an operator's edits survive a re-run.
install_config() {
    if [ -f "$CONFDIR/api.env" ]; then
        log "keeping existing $CONFDIR/api.env"
        return 0
    fi
    $SUDO tee "$CONFDIR/api.env" > /dev/null <<'ENVFILE'
# firecrab API settings. The unit already sets the image root, the dashboard
# assets and the working directory; uncomment only what you want to change.
#
# FIRECRAB_BIND_ADDR=127.0.0.1:3000
# A non-loopback address requires authentication AND TLS to be enabled.
# FIRECRAB_ALLOWED_ORIGINS=
# Empty is correct while the dashboard is served from this same origin.
# FIRECRAB_FIRECRACKER_BIN=firecracker
#
# M2Image install (Images page / POST /api/images/{alias}/install).
# Points at a directory-style base that serves `{alias}.tar.zst` packages
# (see scripts/package-m2images.sh). Unset = use the public MicroRegistry.
# Example local mirror:
# FIRECRAB_IMAGE_BASE_URL=http://127.0.0.1:8765
# Requires host tools: tar, zstd. Restart firecrab-api after changing this.
ENVFILE
    $SUDO chown root:"$FIRECRAB_GROUP" "$CONFDIR/api.env"
    $SUDO chmod 0640 "$CONFDIR/api.env"
    log "wrote $CONFDIR/api.env"
}

# Renders the unit templates with this host's paths and account.
install_units() {
    local uid
    uid=$(id -u "$FIRECRAB_USER")
    for unit in "${UNITS[@]}"; do
        sed -e "s|@LIBDIR@|$LIBDIR|g" \
            -e "s|@SHAREDIR@|$SHAREDIR|g" \
            -e "s|@DATADIR@|$DATADIR|g" \
            -e "s|@CONFDIR@|$CONFDIR|g" \
            -e "s|@FIRECRAB_USER@|$FIRECRAB_USER|g" \
            -e "s|@FIRECRAB_GROUP@|$FIRECRAB_GROUP|g" \
            -e "s|@FIRECRAB_UID@|$uid|g" \
            "$PAYLOAD_UNITS/$unit" | $SUDO tee "$UNITDIR/$unit" > /dev/null
        $SUDO chmod 0644 "$UNITDIR/$unit"
    done
    $SUDO systemctl daemon-reload
    log "units installed to $UNITDIR"
}

# --- guest images ----------------------------------------------------------

ensure_image_build_deps() {
    local failed=0

    ensure chroot coreutils || failed=1
    ensure file file || failed=1
    ensure gzip gzip || failed=1
    ensure install coreutils || failed=1
    ensure mkfs.ext4 e2fsprogs || failed=1
    ensure mount util-linux || failed=1
    ensure curl curl || failed=1
    ensure sha256sum coreutils || failed=1
    ensure tar tar || failed=1
    ensure truncate coreutils || failed=1
    ensure umount util-linux || failed=1
    if [ "$WITH_UBUNTU_IMAGE" -eq 1 ]; then
        ensure find findutils || failed=1
        ensure sed sed || failed=1
        ensure sort coreutils || failed=1
        ensure tail coreutils || failed=1
        ensure ln coreutils || failed=1
        ensure chmod coreutils || failed=1
        ensure chown coreutils || failed=1
    fi
    if [ "$WITH_ROCKY_IMAGE" -eq 1 ]; then
        ensure jq jq || failed=1
        ensure xz xz || failed=1
    fi
    return $failed
}

# Whether a guest rootfs is already installed.
images_present() {
    compgen -G "$DATADIR/images/rootfs/*.ext4" >/dev/null 2>&1
}

image_alias_present() {
    case "$1" in
        alpine*) compgen -G "$DATADIR/images/rootfs/alpine*.ext4" >/dev/null 2>&1 ;;
        ubuntu*) compgen -G "$DATADIR/images/rootfs/ubuntu*.ext4" >/dev/null 2>&1 ;;
        rocky*)  compgen -G "$DATADIR/images/rootfs/rocky*.ext4" >/dev/null 2>&1 ;;
        *) return 1 ;;
    esac
}

# Checkout-only builder. A piped release installer has no such script.
guest_builder() {
    local name=$1
    local script=$REPO_ROOT/scripts/firecracker-menual/$name
    [ -f "$script" ] || return 1
    printf '%s\n' "$script"
}

# Whether the repo has images that can be copied instead of built.
repo_images_present() {
    compgen -G "$REPO_ROOT/images/rootfs/*.ext4" >/dev/null 2>&1
}

want_guest_images() {
    [ "$WITH_IMAGES" -eq 1 ] || [ "$WITH_UBUNTU_IMAGE" -eq 1 ] || [ "$WITH_ROCKY_IMAGE" -eq 1 ]
}

# Report-only counterpart of `ensure_images`, so --check cannot copy, chown or
# build anything (it is run unprivileged, and a check that mutates is a trap).
report_images() {
    if ! want_guest_images; then
        log "images: skipped (install from the dashboard)"
        return 0
    fi
    if repo_images_present; then
        log "images: present in the repo, would be copied to $DATADIR/images"
        return 0
    fi
    if images_present; then
        log "images: already installed in $DATADIR/images"
        return 0
    fi
    if guest_builder install-alpine-rootfs.sh >/dev/null; then
        ensure_image_build_deps || true
        warn "no guest image (would build Alpine with sudo + chroot + direct ext4)"
        return 1
    fi
    warn "no guest image (would install Alpine from MicroRegistry for $(uname -m))"
    return 1
}

# Optional checkout-only image build. Default host install does not do this.
ensure_images() {
    if ! want_guest_images; then
        log "skipping images"
        return 0
    fi

    # Prefer images already built in the repo: copying beats rebuilding.
    if repo_images_present; then
        step "copying images from the repo (existing ones are kept)"
        $SUDO cp -rn "$REPO_ROOT/images/." "$DATADIR/images/" 2>/dev/null || true
        $SUDO chown -R "$FIRECRAB_USER:$FIRECRAB_GROUP" "$DATADIR/images"
        log "images ready in $DATADIR/images"
        return 0
    fi

    if images_present; then
        log "images already installed in $DATADIR/images"
        return 0
    fi

    if [ "$WITH_IMAGES" -eq 1 ]; then
        local alpine
        if ! alpine=$(guest_builder install-alpine-rootfs.sh); then
            log "no local Alpine builder; will install alpine-3.24.1 from MicroRegistry after the API starts"
        else
            if ! ensure_image_build_deps; then
                warn "missing host-native image build dependencies; build elsewhere and copy into $DATADIR/images"
                return 1
            fi
            step "building the Alpine guest image (this takes a few minutes)"
            if ! "$alpine"; then
                warn "image build failed — firecrab will start with no templates; install one from the dashboard"
                return 1
            fi
        fi
    fi

    local ubuntu rocky
    if [ "$WITH_UBUNTU_IMAGE" -eq 1 ]; then
        step "building the Ubuntu guest image (large)"
        if ubuntu=$(guest_builder install-ubuntu-roofs.sh); then
            "$ubuntu" || warn "Ubuntu image build failed (the Alpine one is still usable)"
        else
            warn "Ubuntu builder missing; will try MicroRegistry after the API starts"
        fi
    fi

    if [ "$WITH_ROCKY_IMAGE" -eq 1 ]; then
        step "building the Rocky Linux 9.8 guest image (large)"
        if rocky=$(guest_builder install-rocky-rootfs.sh); then
            "$rocky" || warn "Rocky Linux 9.8 image build failed (the Alpine one is still usable)"
        else
            warn "Rocky builder missing; will try MicroRegistry after the API starts"
        fi
    fi

    $SUDO cp -rn "$REPO_ROOT/images/." "$DATADIR/images/" 2>/dev/null || true
    $SUDO chown -R "$FIRECRAB_USER:$FIRECRAB_GROUP" "$DATADIR/images"
    log "images ready in $DATADIR/images"
}

api_url() {
    printf 'http://%s' "${FIRECRAB_BIND_ADDR:-127.0.0.1:3000}"
}

json_status() {
    printf '%s' "$1" | sed -n 's/.*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1
}

# Poll GET /api/images/{alias}/{package|install} until succeeded or failed.
poll_image_job() {
    local path=$1
    local _i body status
    for _i in $(seq 1 180); do
        body=$(curl -fsS "$(api_url)$path") || return 1
        status=$(json_status "$body")
        case "$status" in
            succeeded) return 0 ;;
            failed)
                warn "$path failed: $body"
                return 1
                ;;
            *) sleep 2 ;;
        esac
    done
    warn "$path did not finish in time"
    return 1
}

# Download + extract one catalog alias via the running API (host arch).
install_catalog_alias() {
    local alias=$1
    local body code
    body=$(mktemp)
    code=$(curl -sS -o "$body" -w '%{http_code}' -X POST "$(api_url)/api/images/$alias/package")
    case "$code" in
        200|202|409) ;;
        *)
            warn "package $alias HTTP $code $(tr '\n' ' ' <"$body")"
            rm -f "$body"
            return 1
            ;;
    esac
    if ! poll_image_job "/api/images/$alias/package"; then
        rm -f "$body"
        return 1
    fi
    code=$(curl -sS -o "$body" -w '%{http_code}' -X POST "$(api_url)/api/images/$alias/install")
    case "$code" in
        200|202) ;;
        409)
            rm -f "$body"
            return 0
            ;;
        *)
            warn "install $alias HTTP $code $(tr '\n' ' ' <"$body")"
            rm -f "$body"
            return 1
            ;;
    esac
    rm -f "$body"
    poll_image_job "/api/images/$alias/install"
}

# After the API is up, install catalog images that were explicitly requested.
install_release_images() {
    want_guest_images || return 0
    local failed=0
    if [ "$WITH_IMAGES" -eq 1 ] && ! image_alias_present alpine; then
        step "installing alpine-3.24.1 from MicroRegistry ($(uname -m))"
        install_catalog_alias alpine-3.24.1 || failed=1
    fi
    if [ "$WITH_UBUNTU_IMAGE" -eq 1 ] && ! image_alias_present ubuntu; then
        step "installing ubuntu-26.04 from MicroRegistry ($(uname -m))"
        install_catalog_alias ubuntu-26.04 || failed=1
    fi
    if [ "$WITH_ROCKY_IMAGE" -eq 1 ] && ! image_alias_present rocky; then
        step "installing rocky-9.8 from MicroRegistry ($(uname -m))"
        install_catalog_alias rocky-9.8 || failed=1
    fi
    if [ "$failed" -eq 0 ] && images_present; then
        log "images ready in $DATADIR/images"
        return 0
    fi
    return 1
}

# --- run -------------------------------------------------------------------

# Enables and starts both units, then confirms they really came up.
start_units() {
    $SUDO systemctl enable "${UNITS[@]}"
    $SUDO systemctl restart "${UNITS[@]}"
    sleep 1
    local failed=0
    for unit in "${UNITS[@]}"; do
        if systemctl is-active --quiet "$unit"; then
            log "$unit is running"
        else
            warn "$unit failed to start — journalctl -u $unit -n 30"
            failed=1
        fi
    done
    if [ "$failed" -eq 0 ]; then
        wait_for_api || failed=1
    fi
    return $failed
}

# `is-active` on a Type=simple unit is true as soon as the process forks —
# not once it is actually serving. At startup firecrab-api hashes every
# present template artifact before binding its listener (see
# TemplateRegistry::from_specs in firecrab-api/src/templates.rs), and a
# freshly built multi-gigabyte rootfs (--with-ubuntu-image, --with-rocky-image)
# can take noticeably longer to hash than the process takes to fork. Poll the
# real HTTP port so callers relying on start_units's return don't race it.
wait_for_api() {
    local bind=${FIRECRAB_BIND_ADDR:-127.0.0.1:3000}
    local _attempt
    for _attempt in $(seq 1 60); do
        curl -fsS -o /dev/null "http://$bind/" && return 0
        sleep 1
    done
    warn "firecrab-api did not answer http://$bind/ within 60s — journalctl -u firecrab-api -n 30"
    return 1
}

# Report-only path: every gap, no changes, no root required.
do_check() {
    log "checking host readiness (nothing will be changed)"
    local gaps=0
    if detect_pkg; then
        log "package manager: $PKG"
    else
        warn "no known package manager (apt/dnf/zypper/pacman/apk)"
        gaps=1
    fi
    ensure_runtime_deps || gaps=1
    ensure_firecracker || gaps=1
    report_payload || gaps=1
    check_kvm || gaps=1
    check_systemd || gaps=1
    report_images || gaps=1
    report_ufw

    if [ "$gaps" -eq 0 ]; then
        log "host is ready — nothing missing"
    else
        warn "run './install.sh' to install the above"
    fi
    step "for runtime host diagnostics (UFW, helper socket, wrong DB path): ./install.sh --doctor"
    return 0
}

# Runtime host diagnostics — delegates to scripts/firecrab-doctor.sh (no root).
do_doctor() {
    local doctor
    if [ -x "$REPO_ROOT/scripts/firecrab-doctor.sh" ]; then
        doctor=$REPO_ROOT/scripts/firecrab-doctor.sh
    elif have firecrab-doctor; then
        doctor=$(command -v firecrab-doctor)
    else
        die "missing firecrab-doctor (run from a checkout, or install first)"
    fi
    # Pass DATADIR through so the doctor matches this install layout.
    DATADIR="$DATADIR" FIRECRAB_API_USER="$FIRECRAB_USER" \
        exec "$doctor" "${DOCTOR_ARGS[@]}"
}

# Installs what firecrab needs and stops there — no account, directories, units
# or services. This is the part that differs per distribution, so it is what a
# container without systemd (and CI's distro matrix) can usefully exercise.
do_deps() {
    require_sudo_ticket
    detect_pkg || die "no known package manager (apt/dnf/zypper/pacman/apk)"

    ensure_runtime_deps || die "missing runtime dependencies (see above)"
    if want_guest_images; then
        ensure_image_build_deps || die "missing image build dependencies (see above)"
    fi
    ensure_firecracker  || die "firecracker is required"
    check_kvm || warn "no KVM here; that only matters where VMs actually run"
    report_ufw
    log "dependencies ready — run without --deps-only to install firecrab"
}

do_print_install_url() {
    firecrab_install_curl_url "${RELEASE_VERSION:-latest}"
}

do_print_release_url() {
    local tarball
    tarball=$(firecrab_host_tarball "" "$LIBC") || die "unsupported architecture: $(uname -m) (need x86_64 or aarch64)"
    firecrab_release_asset_url "$RELEASE_VERSION" "$tarball"
}

# `curl | bash` occupies stdin, so sudo cannot read a password from it.
# Ask once on the real terminal; later $SUDO calls reuse the ticket.
require_sudo_ticket() {
    [ -n "$SUDO" ] || return 0
    sudo -n true >/dev/null 2>&1 && return 0
    [ -e /dev/tty ] || die "sudo needs a password and this session has no terminal"
    step "sudo password required"
    # Redirects attach sudo's prompt to the controlling tty, not stdin.
    # shellcheck disable=SC2024
    sudo -v </dev/tty >/dev/tty 2>/dev/tty || die "sudo authentication failed"
}

# The default path: fill the gaps, fetch binaries, lay out, start.
do_install() {
    require_sudo_ticket
    # Checked before anything is installed: refusing after pulling in five
    # packages would be a waste of the operator's time.
    check_systemd || die "this installer manages systemd units"
    detect_pkg || warn "no known package manager — anything missing has to be installed by hand"

    ensure_runtime_deps || die "missing runtime dependencies (see above)"
    ensure_firecracker  || die "firecracker is required"
    check_kvm || warn "continuing, but VMs will not start until KVM is available"

    resolve_payload
    ensure_account
    ensure_directories
    install_binaries
    label_selinux_binaries
    install_config
    install_units
    # Guest images are not part of the host install. The dashboard Images
    # page (or --with-images / --with-ubuntu-image / --with-rocky-image)
    # installs templates afterwards.
    ensure_images || warn "optional local image build failed"
    start_units || die "installed, but a unit did not come up"
    if want_guest_images; then
        install_release_images || warn "requested catalog image did not install; use the dashboard Images page"
    fi
    report_ufw

    local bind=127.0.0.1:3000
    log "done — dashboard at http://$bind/"
    if ! images_present; then
        step "guest images: install from the dashboard Images page"
    fi
}

# Removes what this script installed; data only with --purge.
do_uninstall() {
    require_sudo_ticket
    for unit in "${UNITS[@]}"; do
        $SUDO systemctl disable --now "$unit" 2>/dev/null || true
        $SUDO rm -f "$UNITDIR/$unit"
    done
    $SUDO systemctl daemon-reload
    log "units removed"

    $SUDO rm -f "$LIBDIR/firecrab-api" "$LIBDIR/firecrab-net-helper"
    $SUDO rm -f "$PREFIX/bin/firecrab-doctor"
    $SUDO rm -rf "$SHAREDIR/dashboard"
    $SUDO rmdir --ignore-fail-on-non-empty "$LIBDIR" "$SHAREDIR" 2>/dev/null || true
    # The file-context rule outlives the directory, so leaving it behind would
    # silently relabel whatever is installed there next.
    if have semanage; then
        $SUDO semanage fcontext -d "${LIBDIR}(/.*)?" 2>/dev/null || true
    fi
    log "binaries and dashboard removed"

    # Left alone on purpose: the account, the config and the data directory.
    # Bridges and nftables tables belong to the helper, which is now stopped;
    # they disappear on the next reboot.
    if [ "$PURGE" -eq 1 ]; then
        warn "purging $DATADIR and $CONFDIR (all VM disks and the database)"
        $SUDO rm -rf "$DATADIR" "$CONFDIR"
    else
        log "kept $DATADIR and $CONFDIR — pass --purge to delete them too"
    fi
}

case "$MODE" in
    check) do_check ;;
    doctor) do_doctor ;;
    deps) do_deps ;;
    print-install-url) do_print_install_url ;;
    print-release-url) do_print_release_url ;;
    install) do_install ;;
    uninstall) do_uninstall ;;
esac
