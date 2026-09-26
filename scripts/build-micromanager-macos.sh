#!/usr/bin/env bash
# Build and ad-hoc sign the Apple Virtualization.framework helper.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'firecrab-micromanager-macos can only be built on macOS' >&2
    exit 1
fi
if (( $# > 1 )); then
    printf 'Usage: %s [output-path]\n' "$0" >&2
    exit 2
fi

output=${1:-"$root/target/release/firecrab-micromanager-macos"}
scratch=${FIRECRAB_MICROMANAGER_SWIFT_SCRATCH:-"$root/target/swift-micromanager"}
package="$root/micromanager-macos"
entitlements="$package/firecrab-micromanager.entitlements"

swift build --package-path "$package" --scratch-path "$scratch" -c release
source_binary=$(swift build --package-path "$package" --scratch-path "$scratch" -c release --show-bin-path)/firecrab-micromanager-macos
mkdir -p "$(dirname -- "$output")"
install -m 0755 "$source_binary" "$output"
codesign --force --sign - --entitlements "$entitlements" "$output"
codesign --verify --strict "$output"
printf '%s\n' "$output"
