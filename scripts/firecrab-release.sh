#!/usr/bin/env bash
# Release URL and payload helpers for install.sh.
# Safe to source: no work at load time.
#
# FIRECRAB_RELEASE_REPO   GitHub owner/name (default SteelCrab/firecrab)
# FIRECRAB_RELEASE_BASE   override the GitHub releases root (tests)

firecrab_release_repo() {
    printf '%s\n' "${FIRECRAB_RELEASE_REPO:-SteelCrab/firecrab}"
}

firecrab_release_base() {
    if [ -n "${FIRECRAB_RELEASE_BASE:-}" ]; then
        printf '%s\n' "$FIRECRAB_RELEASE_BASE"
        return 0
    fi
    printf 'https://github.com/%s/releases\n' "$(firecrab_release_repo)"
}

firecrab_normalize_tag() {
    local version=${1:-}
    if [ -z "$version" ] || [ "$version" = latest ]; then
        printf 'latest\n'
        return 0
    fi
    case "$version" in
        v*) printf '%s\n' "$version" ;;
        *) printf 'v%s\n' "$version" ;;
    esac
}

firecrab_host_arch() {
    local machine=${1:-}
    if [ -z "$machine" ]; then
        machine=$(uname -m 2>/dev/null || printf 'unknown')
    fi
    case "$machine" in
        x86_64|amd64) printf 'x86_64\n' ;;
        aarch64|arm64) printf 'aarch64\n' ;;
        *) return 1 ;;
    esac
}

firecrab_normalize_libc() {
    local libc=${1:-}
    case "$libc" in
        gnu|glibc) printf 'gnu\n' ;;
        musl) printf 'musl\n' ;;
        "") return 1 ;;
        *) return 1 ;;
    esac
}

# musl hosts (Alpine) get the musl bundle; everyone else gets glibc/gnu.
# FIRECRAB_LIBC or an explicit argument wins.
firecrab_host_libc() {
    local libc=${1:-${FIRECRAB_LIBC:-}}
    if [ -n "$libc" ]; then
        firecrab_normalize_libc "$libc"
        return
    fi
    if [ -e /lib/ld-musl-x86_64.so.1 ] || [ -e /lib/ld-musl-aarch64.so.1 ]; then
        printf 'musl\n'
        return 0
    fi
    if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
        printf 'musl\n'
        return 0
    fi
    printf 'gnu\n'
}

firecrab_host_tarball() {
    local arch libc
    arch=$(firecrab_host_arch "${1:-}") || return 1
    libc=$(firecrab_host_libc "${2:-}") || return 1
    printf 'firecrab-host-%s-%s.tar.gz\n' "$arch" "$libc"
}

# Architectures and libcs the release host bundle is built for.
# install.sh picks one of the four: {x86_64,aarch64} × {gnu,musl}.
firecrab_supported_arches() {
    printf '%s\n' x86_64 aarch64
}

firecrab_supported_libcs() {
    printf '%s\n' gnu musl
}

firecrab_supported_host_tarballs() {
    local arch libc
    for arch in $(firecrab_supported_arches); do
        for libc in $(firecrab_supported_libcs); do
            printf 'firecrab-host-%s-%s.tar.gz\n' "$arch" "$libc"
        done
    done
}

# Read ELF e_machine (little-endian 64-bit) and map to a host arch name.
firecrab_elf_arch() {
    local path=$1
    local class data machine
    [ -f "$path" ] || return 1
    class=$(od -An -t u1 -j 4 -N 1 -- "$path" | tr -d ' ')
    data=$(od -An -t u1 -j 5 -N 1 -- "$path" | tr -d ' ')
    [ "$class" = 2 ] && [ "$data" = 1 ] || return 1
    machine=$(od -An -t u2 -j 18 -N 2 -- "$path" | tr -d ' ')
    case "$machine" in
        62) printf 'x86_64\n' ;;
        183) printf 'aarch64\n' ;;
        *) return 1 ;;
    esac
}

firecrab_assert_binary_arch() {
    local path=$1 want=$2
    local got
    got=$(firecrab_elf_arch "$path") || return 1
    [ "$got" = "$want" ]
}

firecrab_release_asset_url() {
    local version=$1 asset=$2
    local tag base
    tag=$(firecrab_normalize_tag "$version")
    base=$(firecrab_release_base)
    if [ "$tag" = latest ]; then
        printf '%s/latest/download/%s\n' "$base" "$asset"
    else
        printf '%s/download/%s/%s\n' "$base" "$tag" "$asset"
    fi
}

firecrab_install_curl_url() {
    local version=${1:-latest}
    printf 'curl -fsSL %s | bash\n' "$(firecrab_release_asset_url "$version" install.sh)"
}

firecrab_payload_mode() {
    if [ -n "${1:-}" ]; then
        printf 'dir\n'
    else
        printf 'release\n'
    fi
}

# Print the path to use for `name`: --bin-dir wins when the file is there,
# otherwise the already-installed copy. Missing on both sides is an error.
firecrab_resolve_binary() {
    local name=$1 src_dir=$2 dest_dir=$3
    if [ -n "$src_dir" ] && [ -x "$src_dir/$name" ]; then
        printf '%s\n' "$src_dir/$name"
        return 0
    fi
    if [ -x "$dest_dir/$name" ]; then
        printf '%s\n' "$dest_dir/$name"
        return 0
    fi
    return 1
}

firecrab_verify_sha256() {
    local sums=$1 file=$2
    local name want got
    name=$(basename -- "$file")
    want=$(awk -v n="$name" '
        $2 == n || $2 == "*" n || $2 ~ "/" n "$" { print $1; exit }
    ' "$sums")
    [ -n "$want" ] || return 1
    got=$(sha256sum -- "$file" | awk '{ print $1 }')
    [ "$want" = "$got" ]
}
