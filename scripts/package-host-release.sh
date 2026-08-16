#!/usr/bin/env bash
# Assemble firecrab-host-$arch.tar.gz from musl binaries + dashboard + host files.
set -euo pipefail

usage() {
    printf 'Usage: %s <arch> <bin-dir> <dashboard-dir> <output.tar.gz>\n' "$0" >&2
    exit 2
}

[ $# -eq 4 ] || usage
arch=$1
bin_dir=$2
dashboard_dir=$3
output=$4

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck disable=SC1091
. "$root/scripts/firecrab-release.sh"

case "$arch" in
    x86_64|aarch64) ;;
    *) printf 'unsupported arch: %s\n' "$arch" >&2; exit 1 ;;
esac

[ -x "$bin_dir/firecrab-api" ] || { printf 'missing %s\n' "$bin_dir/firecrab-api" >&2; exit 1; }
[ -x "$bin_dir/firecrab-net-helper" ] || { printf 'missing %s\n' "$bin_dir/firecrab-net-helper" >&2; exit 1; }
if ! firecrab_assert_binary_arch "$bin_dir/firecrab-api" "$arch"; then
    printf '%s is not a %s ELF (wrong architecture)\n' "$bin_dir/firecrab-api" "$arch" >&2
    exit 1
fi
if ! firecrab_assert_binary_arch "$bin_dir/firecrab-net-helper" "$arch"; then
    printf '%s is not a %s ELF (wrong architecture)\n' "$bin_dir/firecrab-net-helper" "$arch" >&2
    exit 1
fi
[ -f "$dashboard_dir/index.html" ] || { printf 'missing %s/index.html\n' "$dashboard_dir" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/systemd" "$stage/dashboard"

install -m 0755 "$bin_dir/firecrab-api" "$stage/firecrab-api"
install -m 0755 "$bin_dir/firecrab-net-helper" "$stage/firecrab-net-helper"
install -m 0755 "$root/scripts/firecracker-menual/extract-vmlinux" "$stage/extract-vmlinux"
install -m 0755 "$root/scripts/firecracker-menual/extract-arm64-image" "$stage/extract-arm64-image"
install -m 0755 "$root/scripts/firecrab-doctor.sh" "$stage/firecrab-doctor.sh"
install -m 0755 "$root/scripts/firecrab.sh" "$stage/firecrab.sh"
cp "$root/packaging/systemd/"*.service "$stage/systemd/"
cp -a "$dashboard_dir/." "$stage/dashboard/"

mkdir -p "$(dirname -- "$output")"
tar -C "$stage" -czf "$output" \
    firecrab-api firecrab-net-helper extract-vmlinux extract-arm64-image \
    firecrab-doctor.sh firecrab.sh systemd dashboard
printf '%s\n' "$output"
