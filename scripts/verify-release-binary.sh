#!/usr/bin/env bash
# Confirm a release binary matches the requested host arch and libc.
# musl bundles must be static. gnu/glibc bundles are dynamically linked.
set -euo pipefail

[ $# -eq 2 ] || [ $# -eq 3 ] || {
    printf 'Usage: %s <binary> <x86_64|aarch64> [gnu|musl]\n' "$0" >&2
    exit 2
}
path=$1
arch=$2
libc=$(printf '%s\n' "${3:-musl}")

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck disable=SC1091
. "$root/scripts/firecrab-release.sh"

libc=$(firecrab_normalize_libc "$libc") \
    || { printf 'libc must be gnu or musl\n' >&2; exit 1; }

[ -x "$path" ] || { printf 'not executable: %s\n' "$path" >&2; exit 1; }
firecrab_assert_binary_arch "$path" "$arch" \
    || { printf '%s is not a %s ELF\n' "$path" "$arch" >&2; exit 1; }

if command -v ldd >/dev/null 2>&1; then
    linked=$(ldd -- "$path" 2>&1 || true)
    case "$libc" in
        musl)
            if printf '%s\n' "$linked" | grep -q '=>'; then
                printf '%s is dynamically linked; the musl host bundle must be static\n' "$path" >&2
                printf '%s\n' "$linked" >&2
                exit 1
            fi
            ;;
        gnu)
            if ! printf '%s\n' "$linked" | grep -q 'libc.so.6'; then
                printf '%s is not a glibc binary (expected libc.so.6)\n' "$path" >&2
                printf '%s\n' "$linked" >&2
                exit 1
            fi
            ;;
    esac
fi

printf '%s: %s %s\n' "$path" "$arch" "$libc"
