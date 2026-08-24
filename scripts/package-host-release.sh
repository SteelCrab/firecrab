#!/usr/bin/env bash
# Assemble a FireCrab host bundle from binaries, dashboard, host files, and
# release-compliance material.
set -euo pipefail

usage() {
    printf 'Usage: %s <arch> <bin-dir> <dashboard-dir> <output.tar.gz> [compliance-dir]\n' "$0" >&2
    exit 2
}

if (( $# < 4 || $# > 5 )); then
    usage
fi
arch=$1
bin_dir=$2
dashboard_dir=$3
output=$4
compliance_dir=${5:-}

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck disable=SC1091
. "$root/scripts/firecrab-release.sh"

case "$arch" in
    x86_64|aarch64) ;;
    *) printf 'unsupported arch: %s\n' "$arch" >&2; exit 1 ;;
esac

[ -x "$bin_dir/firecrab-api" ] || { printf 'missing %s\n' "$bin_dir/firecrab-api" >&2; exit 1; }
[ -x "$bin_dir/firecrab-net-helper" ] || { printf 'missing %s\n' "$bin_dir/firecrab-net-helper" >&2; exit 1; }
[ -x "$bin_dir/firecrab" ] || { printf 'missing %s\n' "$bin_dir/firecrab" >&2; exit 1; }
if ! firecrab_assert_binary_arch "$bin_dir/firecrab-api" "$arch"; then
    printf '%s is not a %s ELF (wrong architecture)\n' "$bin_dir/firecrab-api" "$arch" >&2
    exit 1
fi
if ! firecrab_assert_binary_arch "$bin_dir/firecrab-net-helper" "$arch"; then
    printf '%s is not a %s ELF (wrong architecture)\n' "$bin_dir/firecrab-net-helper" "$arch" >&2
    exit 1
fi
if ! firecrab_assert_binary_arch "$bin_dir/firecrab" "$arch"; then
    printf '%s is not a %s ELF (wrong architecture)\n' "$bin_dir/firecrab" "$arch" >&2
    exit 1
fi
[ -f "$dashboard_dir/index.html" ] || { printf 'missing %s/index.html\n' "$dashboard_dir" >&2; exit 1; }
[ -f "$root/LICENSE" ] || { printf 'missing %s/LICENSE\n' "$root" >&2; exit 1; }
[ -f "$root/licenses/GPL-2.0-only.txt" ] || {
    printf 'missing %s/licenses/GPL-2.0-only.txt\n' "$root" >&2
    exit 1
}

if [ -n "$compliance_dir" ]; then
    [ -f "$compliance_dir/THIRD_PARTY_NOTICES.txt" ] || {
        printf 'missing %s/THIRD_PARTY_NOTICES.txt\n' "$compliance_dir" >&2
        exit 1
    }
    [ -f "$compliance_dir/release-license-inventory.json" ] || {
        printf 'missing %s/release-license-inventory.json\n' "$compliance_dir" >&2
        exit 1
    }
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/systemd" "$stage/dashboard" "$stage/licenses"

install -m 0755 "$bin_dir/firecrab-api" "$stage/firecrab-api"
install -m 0755 "$bin_dir/firecrab-net-helper" "$stage/firecrab-net-helper"
install -m 0755 "$bin_dir/firecrab" "$stage/firecrab"
install -m 0755 "$root/scripts/firecracker-menual/extract-vmlinux" "$stage/extract-vmlinux"
install -m 0755 "$root/scripts/firecracker-menual/extract-arm64-image" "$stage/extract-arm64-image"
install -m 0644 "$root/LICENSE" "$stage/LICENSE"
install -m 0644 "$root/licenses/GPL-2.0-only.txt" "$stage/licenses/GPL-2.0-only.txt"
if [ -n "$compliance_dir" ]; then
    install -m 0644 "$compliance_dir/THIRD_PARTY_NOTICES.txt" "$stage/THIRD_PARTY_NOTICES.txt"
    install -m 0644 "$compliance_dir/release-license-inventory.json" "$stage/release-license-inventory.json"
fi
cp "$root/packaging/systemd/"*.service "$stage/systemd/"
cp -a "$dashboard_dir/." "$stage/dashboard/"

mkdir -p "$(dirname -- "$output")"
members=(
    firecrab-api firecrab-net-helper extract-vmlinux extract-arm64-image
    firecrab systemd dashboard LICENSE licenses
)
if [ -n "$compliance_dir" ]; then
    members+=(THIRD_PARTY_NOTICES.txt release-license-inventory.json)
fi
tar -C "$stage" -czf "$output" "${members[@]}"
printf '%s\n' "$output"
