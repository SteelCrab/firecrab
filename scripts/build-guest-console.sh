#!/usr/bin/env bash
# Build a guest helper that also runs in musl and distroless images.
set -euo pipefail
[ "$#" -eq 1 ] || { printf 'Usage: %s <output-directory>\n' "$0" >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { printf 'build guest console on Linux with musl-tools\n' >&2; exit 1; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$1/guest-console"
output=$(cd -- "$1" && pwd)
cc=${CC:-musl-gcc}
license=${GUEST_CONSOLE_LIBC_LICENSE:-/usr/share/doc/musl/copyright}
[ -f "$license" ] || { printf 'missing musl license: %s\n' "$license" >&2; exit 1; }
uapi_arch=$("$cc" -dumpmachine)
"$cc" -idirafter /usr/include -idirafter "/usr/include/$uapi_arch" -std=c11 -Os -static -Wall -Wextra -Werror \
    "$root/packaging/guest-console/main.c" "$root/packaging/guest-console/resize.c" \
    "$root/packaging/guest-console/user.c" \
    "$root/packaging/guest-console/app_log.c" \
    "$root/packaging/guest-console/workspace.c" \
    -o "$output/firecrab-guest-console"
"$root/scripts/verify-release-binary.sh" "$output/firecrab-guest-console" "$(uname -m)" musl
"$output/firecrab-guest-console" --version
install -m 0644 "$license" "$output/guest-console/LICENSE.musl"
