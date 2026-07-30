#!/usr/bin/env bash
# firecrab host installer (docs/30-tasks/task-host-install-script.md).
#
# One command on a machine that only has network access: works out what is
# missing, installs it, builds firecrab, and leaves two systemd daemons
# running with the dashboard served on http://127.0.0.1:3000/.
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

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
UNITS=(firecrab-net-helper.service firecrab-api.service)

MODE=install
INSTALL_DEPS=1
WITH_IMAGES=1
WITH_UBUNTU_IMAGE=0
WITH_FRONTEND=1
PURGE=0

PKG=''          # detected package manager
PKG_UPDATED=0   # index refreshed at most once

# Help text, also printed before failing on an unknown option.
usage() {
    cat <<'USAGE'
Usage: sudo ./install.sh [OPTIONS]

  (no options)        install everything that is missing, then start firecrab
  --check             report what is missing and what would be installed
  --deps-only         install the dependencies, then stop (no host changes)
  --no-deps           never install packages/toolchains, only report gaps
  --no-images         do not build a guest image
  --with-ubuntu-image also build the Ubuntu image (large, needs root chroot)
  --no-frontend       do not build/install the dashboard
  --uninstall         stop and remove units, binaries and dashboard
  --purge             with --uninstall, also delete /var/lib/firecrab (VM data!)
  -h, --help          this text

Environment: FIRECRAB_USER, FIRECRAB_GROUP, PREFIX, DATADIR, CONFDIR, UNITDIR
USAGE
}

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
step() { printf '\033[1;36m--\033[0m  %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }
need_root() { [ "$(id -u)" -eq 0 ] || die "run as root: sudo ./install.sh $*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --check) MODE=check ;;
        --deps-only) MODE=deps ;;
        --no-deps) INSTALL_DEPS=0 ;;
        --no-images) WITH_IMAGES=0 ;;
        --with-ubuntu-image) WITH_UBUNTU_IMAGE=1 ;;
        --no-frontend) WITH_FRONTEND=0 ;;
        --uninstall) MODE=uninstall ;;
        --purge) PURGE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
    shift
done

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
        apk:e2fsprogs) echo "e2fsprogs-extra" ;;
        *:e2fsprogs)  echo "e2fsprogs" ;;
        *:curl)       echo "curl" ;;
        # scripts/install-firecracker.sh needs find and tar; a minimal
        # openSUSE or Debian install has neither.
        *:findutils)  echo "findutils" ;;
        *:tar)        echo "tar" ;;
        # cargo needs a linker; rustup only warns that one is missing and the
        # build then fails at link time with a much less obvious error.
        apt-get:cc)   echo "build-essential" ;;
        apk:cc)       echo "build-base" ;;
        *:cc)         echo "gcc" ;;
        # Node is the one dependency nobody names the same way. openSUSE has
        # meta packages; Fedora ships only versioned streams (nodejs22-npm-bin
        # and friends), so the unversioned guess below simply fails there and
        # `ensure_build_tools` carries on without the dashboard.
        zypper:node)  echo "nodejs-default npm-default" ;;
        *:node)       echo "nodejs npm" ;;
        apt-get:docker) echo "docker.io" ;;
        *:docker)     echo "docker" ;;
        *) echo "$generic" ;;
    esac
}

# Refreshes the package index at most once per run.
pkg_refresh() {
    [ "$PKG_UPDATED" -eq 1 ] && return 0
    case "$PKG" in
        apt-get) DEBIAN_FRONTEND=noninteractive apt-get update -qq ;;
        apk) apk update -q ;;
        *) : ;;  # dnf/zypper/pacman refresh as part of install
    esac
    PKG_UPDATED=1
}

# Installs the named packages with whichever manager was detected.
pkg_install() {
    pkg_refresh
    case "$PKG" in
        apt-get) DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" ;;
        dnf) dnf install -y -q "$@" ;;
        zypper) zypper --non-interactive install -y "$@" ;;
        pacman) pacman -Sy --noconfirm --needed "$@" ;;
        apk) apk add --no-cache "$@" ;;
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

# UFW is not ours to change, so its DHCP block is reported, not fixed.
report_ufw() {
    have ufw || return 0
    local status
    if status=$(ufw status 2>/dev/null); then
        case "$status" in
            *"Status: active"*)
                warn "UFW is active — allow DHCP/DNS on each MicroNetwork bridge (docs/20-guides/troubleshooting.md)" ;;
            *) log "UFW installed but inactive" ;;
        esac
    else
        warn "UFW installed; re-run as root to see whether it is active (it blocks new bridges' DHCP)"
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
    ensure mkfs.ext4 e2fsprogs || failed=1
    ensure curl curl     || failed=1
    ensure find findutils || failed=1
    ensure tar tar       || failed=1
    return $failed
}

# Comes from its own upstream script rather than a distro package.
ensure_firecracker() {
    have firecracker && { log "firecracker present"; return 0; }

    if [ "$MODE" = check ]; then
        warn "missing: firecracker (would install via scripts/install-firecracker.sh)"
        return 1
    fi
    [ "$INSTALL_DEPS" -eq 1 ] || { warn "missing: firecracker (--no-deps; run scripts/install-firecracker.sh)"; return 1; }

    step "installing firecracker"
    "$REPO_ROOT/scripts/install-firecracker.sh" || { warn "firecracker install failed"; return 1; }
    have firecracker
}

# cargo, plus npm when the dashboard is being built.
ensure_build_tools() {
    local failed=0

    if have cargo; then
        log "cargo present"
    elif [ "$MODE" = check ]; then
        warn "missing: cargo (would install via rustup)"
        failed=1
    elif [ "$INSTALL_DEPS" -eq 1 ]; then
        step "installing the Rust toolchain (rustup)"
        # Downloaded whole, then run - piping curl into sh executes whatever
        # arrived if the transfer is cut off partway.
        local installer
        installer=$(mktemp)
        # shellcheck disable=SC2064 # expand the path now, not at trap time
        trap "rm -f '$installer'" RETURN
        if curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$installer"; then
            sh "$installer" -y --no-modify-path || { warn "rustup install failed"; failed=1; }
        else
            warn "could not download the rustup installer"
            failed=1
        fi
        # rustup lands in $HOME/.cargo for whoever runs this (root under sudo).
        export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
        have cargo || { warn "cargo still not on PATH"; failed=1; }
    else
        warn "missing: cargo (--no-deps; install rustup yourself)"
        failed=1
    fi

    ensure cc cc || failed=1

    if [ "$WITH_FRONTEND" -eq 1 ] && ! ensure npm node; then
        # Degraded, not fatal: the API and MicroNetworks work without the
        # dashboard, and node package names vary too much between distributions
        # to fail an install over. Build it later and re-run.
        warn "no npm — installing without the dashboard (re-run after installing Node)"
        WITH_FRONTEND=0
    fi
    return $failed
}

# --- build -----------------------------------------------------------------

# Release binaries, and the dashboard bundle unless it was skipped.
build_all() {
    step "building the workspace (release)"
    ( cd "$REPO_ROOT" && cargo build --release --workspace )

    if [ "$WITH_FRONTEND" -eq 1 ]; then
        step "building the dashboard"
        ( cd "$REPO_ROOT/firecrab-frontend" && npm ci && npm run build )
    fi
}

# --- host layout -----------------------------------------------------------

# The account the API runs as, in the kvm group for firecracker.
ensure_account() {
    if getent group "$FIRECRAB_GROUP" >/dev/null; then
        log "group $FIRECRAB_GROUP exists"
    else
        step "creating group $FIRECRAB_GROUP"
        groupadd --system "$FIRECRAB_GROUP"
    fi

    if id "$FIRECRAB_USER" >/dev/null 2>&1; then
        log "user $FIRECRAB_USER exists"
    else
        step "creating user $FIRECRAB_USER"
        useradd --system --gid "$FIRECRAB_GROUP" --home-dir "$DATADIR" \
                --shell /usr/sbin/nologin "$FIRECRAB_USER"
    fi

    # The API spawns firecracker, which opens /dev/kvm.
    if getent group kvm >/dev/null && ! id -nG "$FIRECRAB_USER" | tr ' ' '\n' | grep -qx kvm; then
        step "adding $FIRECRAB_USER to the kvm group"
        usermod --append --groups kvm "$FIRECRAB_USER"
    fi
}

# Data, config and install directories with their intended owners.
ensure_directories() {
    install -d -o root -g root -m 0755 "$LIBDIR" "$SHAREDIR"
    install -d -o "$FIRECRAB_USER" -g "$FIRECRAB_GROUP" -m 0750 \
            "$DATADIR" "$DATADIR/data" "$DATADIR/images"
    install -d -o root -g "$FIRECRAB_GROUP" -m 0750 "$CONFDIR"
    log "directories ready under $DATADIR, $CONFDIR"
}

# Puts the built binaries, and the dashboard, where the units expect them.
install_binaries() {
    local target="$REPO_ROOT/target/release"
    for binary in firecrab-api firecrab-net-helper; do
        [ -x "$target/$binary" ] || die "$target/$binary not found — the build did not produce it"
        install -o root -g root -m 0755 "$target/$binary" "$LIBDIR/$binary"
    done
    log "binaries installed to $LIBDIR"

    [ "$WITH_FRONTEND" -eq 1 ] || return 0
    local dist="$REPO_ROOT/firecrab-frontend/dist"
    [ -f "$dist/index.html" ] || die "$dist/index.html not found — the dashboard build did not produce it"
    rm -rf "$SHAREDIR/dashboard"
    install -d -o root -g root -m 0755 "$SHAREDIR/dashboard"
    cp -r "$dist/." "$SHAREDIR/dashboard/"
    chown -R root:root "$SHAREDIR/dashboard"
    log "dashboard installed to $SHAREDIR/dashboard"
}

# Seeds api.env once so an operator's edits survive a re-run.
install_config() {
    if [ -f "$CONFDIR/api.env" ]; then
        log "keeping existing $CONFDIR/api.env"
        return 0
    fi
    cat > "$CONFDIR/api.env" <<'ENVFILE'
# firecrab API settings. The unit already sets the image root, the dashboard
# assets and the working directory; uncomment only what you want to change.
#
# FIRECRAB_BIND_ADDR=127.0.0.1:3000
# A non-loopback address requires authentication AND TLS to be enabled.
# FIRECRAB_ALLOWED_ORIGINS=
# Empty is correct while the dashboard is served from this same origin.
# FIRECRAB_FIRECRACKER_BIN=firecracker
ENVFILE
    chown root:"$FIRECRAB_GROUP" "$CONFDIR/api.env"
    chmod 0640 "$CONFDIR/api.env"
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
            "$REPO_ROOT/packaging/systemd/$unit" > "$UNITDIR/$unit"
        chmod 0644 "$UNITDIR/$unit"
    done
    systemctl daemon-reload
    log "units installed to $UNITDIR"
}

# --- guest images ----------------------------------------------------------

# Whether a guest rootfs is already installed.
images_present() {
    compgen -G "$DATADIR/images/rootfs/*.ext4" >/dev/null 2>&1
}

# Whether the repo has images that can be copied instead of built.
repo_images_present() {
    compgen -G "$REPO_ROOT/images/rootfs/*.ext4" >/dev/null 2>&1
}

# Report-only counterpart of `ensure_images`, so --check cannot copy, chown or
# build anything (it is run unprivileged, and a check that mutates is a trap).
report_images() {
    if [ "$WITH_IMAGES" -eq 0 ]; then
        log "images: skipped (--no-images)"
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
    if have docker; then
        warn "no guest image (would build the Alpine image with docker)"
    else
        warn "no guest image (would install docker, then build the Alpine image)"
    fi
    return 1
}

# Gets a guest image into place: copy from the repo, otherwise build one.
ensure_images() {
    [ "$WITH_IMAGES" -eq 1 ] || { log "skipping images (--no-images)"; return 0; }

    # Prefer images already built in the repo: copying beats rebuilding.
    if repo_images_present; then
        step "copying images from the repo (existing ones are kept)"
        cp -rn "$REPO_ROOT/images/." "$DATADIR/images/" 2>/dev/null || true
        chown -R "$FIRECRAB_USER:$FIRECRAB_GROUP" "$DATADIR/images"
        log "images ready in $DATADIR/images"
        return 0
    fi

    if images_present; then
        log "images already installed in $DATADIR/images"
        return 0
    fi

    # Nothing to copy: build one. Alpine only by default — it is ~10x smaller
    # than Ubuntu's and builds in a container, needing no host chroot.
    if ! ensure docker docker; then
        warn "no docker — cannot build a guest image. Build it elsewhere and copy it into $DATADIR/images"
        return 1
    fi
    if ! systemctl is-active --quiet docker 2>/dev/null; then
        step "starting docker"
        systemctl enable --now docker >/dev/null 2>&1 || warn "could not start docker"
    fi

    step "building the Alpine guest image (this takes a few minutes)"
    if ! "$REPO_ROOT/scripts/firecracker-menual/install-alpine-rootfs.sh"; then
        warn "image build failed — firecrab will start with no templates; build one later and copy it in"
        return 1
    fi

    if [ "$WITH_UBUNTU_IMAGE" -eq 1 ]; then
        step "building the Ubuntu guest image (large)"
        "$REPO_ROOT/scripts/firecracker-menual/install-ubuntu-roofs.sh" \
            || warn "Ubuntu image build failed (the Alpine one is still usable)"
    fi

    cp -rn "$REPO_ROOT/images/." "$DATADIR/images/" 2>/dev/null || true
    chown -R "$FIRECRAB_USER:$FIRECRAB_GROUP" "$DATADIR/images"
    log "images ready in $DATADIR/images"
}

# --- run -------------------------------------------------------------------

# Enables and starts both units, then confirms they really came up.
start_units() {
    systemctl enable --now "${UNITS[@]}"
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
    return $failed
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
    ensure_build_tools || gaps=1
    check_kvm || gaps=1
    check_systemd || gaps=1
    report_images || gaps=1
    report_ufw

    if [ "$gaps" -eq 0 ]; then
        log "host is ready — nothing missing"
    else
        warn "run 'sudo ./install.sh' to install the above"
    fi
    return 0
}

# Installs what firecrab needs and stops there — no account, directories, units
# or services. This is the part that differs per distribution, so it is what a
# container without systemd (and CI's distro matrix) can usefully exercise.
do_deps() {
    need_root --deps-only
    detect_pkg || die "no known package manager (apt/dnf/zypper/pacman/apk)"

    ensure_runtime_deps || die "missing runtime dependencies (see above)"
    ensure_firecracker  || die "firecracker is required"
    ensure_build_tools  || die "build tools are required"
    check_kvm || warn "no KVM here; that only matters where VMs actually run"
    report_ufw
    log "dependencies ready — run without --deps-only to install firecrab"
}

# The default path: fill the gaps, build, lay out, start.
do_install() {
    need_root
    # Checked before anything is installed: refusing after pulling in five
    # packages would be a waste of the operator's time.
    check_systemd || die "this installer manages systemd units"
    detect_pkg || warn "no known package manager — anything missing has to be installed by hand"

    ensure_runtime_deps || die "missing runtime dependencies (see above)"
    ensure_firecracker  || die "firecracker is required"
    ensure_build_tools  || die "build tools are required"
    check_kvm || warn "continuing, but VMs will not start until KVM is available"

    build_all
    ensure_account
    ensure_directories
    install_binaries
    install_config
    install_units
    # Deliberately non-fatal: the API now starts with no templates, and an
    # image can be built or copied in afterwards.
    ensure_images || warn "no guest image yet — the dashboard works, VM creation needs one"
    start_units || die "installed, but a unit did not come up"
    report_ufw

    local bind=127.0.0.1:3000
    log "done — dashboard at http://$bind/"
    if ! images_present; then
        warn "no template image installed; build one with scripts/firecracker-menual/install-alpine-rootfs.sh and copy it to $DATADIR/images"
    fi
}

# Removes what this script installed; data only with --purge.
do_uninstall() {
    need_root --uninstall
    for unit in "${UNITS[@]}"; do
        systemctl disable --now "$unit" 2>/dev/null || true
        rm -f "$UNITDIR/$unit"
    done
    systemctl daemon-reload
    log "units removed"

    rm -f "$LIBDIR/firecrab-api" "$LIBDIR/firecrab-net-helper"
    rm -rf "$SHAREDIR/dashboard"
    rmdir --ignore-fail-on-non-empty "$LIBDIR" "$SHAREDIR" 2>/dev/null || true
    log "binaries and dashboard removed"

    # Left alone on purpose: the account, the config and the data directory.
    # Bridges and nftables tables belong to the helper, which is now stopped;
    # they disappear on the next reboot.
    if [ "$PURGE" -eq 1 ]; then
        warn "purging $DATADIR and $CONFDIR (all VM disks and the database)"
        rm -rf "$DATADIR" "$CONFDIR"
    else
        log "kept $DATADIR and $CONFDIR — pass --purge to delete them too"
    fi
}

case "$MODE" in
    check) do_check ;;
    deps) do_deps ;;
    install) do_install ;;
    uninstall) do_uninstall ;;
esac
