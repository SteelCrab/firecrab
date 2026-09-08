#!/bin/sh
# Install only the cross-platform firecrab client into a user-writable directory.
set -eu

RELEASE_BASE=${FIRECRAB_RELEASE_BASE:-https://github.com/SteelCrab/firecrab/releases}
VERSION=${FIRECRAB_VERSION:-latest}
INSTALL_DIR=${FIRECRAB_INSTALL_DIR:-}
CHECK=0
PRINT_ASSET=0
PRINT_URL=0

usage() {
    cat <<'EOF'
Usage: install-cli.sh [options]

Install the firecrab client without root, Cargo, or the Linux host services.

Options:
  --version VER       Release tag such as v0.1.3 (default: latest)
  --install-dir DIR   Destination directory (default: ~/.local/bin)
  --check             Print the selected asset and destination without changes
  --print-asset       Print the selected release asset name
  --print-url         Print the selected release asset URL
  -h, --help          Show this help

Environment:
  FIRECRAB_RELEASE_BASE  Alternate release root for mirrors or tests
  FIRECRAB_TEST_ALLOW_FILE_URL=1
                         Permit file:// release roots in tests only
  FIRECRAB_VERSION       Default release tag
  FIRECRAB_INSTALL_DIR   Default destination directory
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { printf '%s\n' '--version needs a value' >&2; exit 2; }
            VERSION=$2
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || { printf '%s\n' '--install-dir needs a value' >&2; exit 2; }
            INSTALL_DIR=$2
            shift 2
            ;;
        --check) CHECK=1; shift ;;
        --print-asset) PRINT_ASSET=1; shift ;;
        --print-url) PRINT_URL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$INSTALL_DIR" ]; then
    [ -n "${HOME:-}" ] || { printf '%s\n' 'HOME is not set; pass --install-dir or FIRECRAB_INSTALL_DIR' >&2; exit 1; }
    INSTALL_DIR="$HOME/.local/bin"
fi
RELEASE_BASE=${RELEASE_BASE%/}
case "$RELEASE_BASE" in
    https://?*) ;;
    file://?*)
        [ "${FIRECRAB_TEST_ALLOW_FILE_URL:-}" = 1 ] \
            || { printf '%s\n' 'file:// release roots require FIRECRAB_TEST_ALLOW_FILE_URL=1' >&2; exit 1; }
        ;;
    *)
        printf '%s\n' 'FIRECRAB_RELEASE_BASE must use HTTPS' >&2
        exit 1
        ;;
esac

raw_os=${FIRECRAB_CLI_OS:-$(uname -s 2>/dev/null || printf unknown)}
case "$raw_os" in
    Linux|linux) platform=linux ;;
    Darwin|darwin|macOS|macos) platform=macos ;;
    *) printf 'unsupported client OS: %s\n' "$raw_os" >&2; exit 1 ;;
esac

raw_arch=${FIRECRAB_CLI_ARCH:-$(uname -m 2>/dev/null || printf unknown)}
case "$raw_arch" in
    x86_64|amd64|X64) architecture=x86_64 ;;
    aarch64|arm64|Arm64) architecture=aarch64 ;;
    *) printf 'unsupported client architecture: %s\n' "$raw_arch" >&2; exit 1 ;;
esac

asset="firecrab-cli-${architecture}-${platform}.tar.gz"
case "$VERSION" in
    ''|latest) download_root="$RELEASE_BASE/latest/download" ;;
    v*) download_root="$RELEASE_BASE/download/$VERSION" ;;
    *) download_root="$RELEASE_BASE/download/v$VERSION" ;;
esac
asset_url="$download_root/$asset"

[ "$PRINT_ASSET" -eq 0 ] || { printf '%s\n' "$asset"; exit 0; }
[ "$PRINT_URL" -eq 0 ] || { printf '%s\n' "$asset_url"; exit 0; }
if [ "$CHECK" -eq 1 ]; then
    printf 'would install %s to %s/firecrab\n' "$asset_url" "$INSTALL_DIR"
    exit 0
fi

command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required' >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { printf '%s\n' 'tar is required' >&2; exit 1; }

temporary=$(mktemp -d 2>/dev/null || mktemp -d -t firecrab-cli)
temporary_binary=
cleanup() {
    rm -rf "$temporary"
    [ -z "$temporary_binary" ] || rm -f "$temporary_binary"
}
trap cleanup EXIT HUP INT TERM
curl -fsSL "$asset_url" -o "$temporary/$asset"
curl -fsSL "$download_root/SHA256SUMS" -o "$temporary/SHA256SUMS"

expected=$(awk -v name="$asset" '
    $2 == name || $2 == "*" name || $2 ~ "/" name "$" { print $1; exit }
' "$temporary/SHA256SUMS")
[ -n "$expected" ] || { printf 'checksum missing for %s\n' "$asset" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$asset" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$asset" | awk '{ print $1 }')
else
    printf '%s\n' 'sha256sum or shasum is required' >&2
    exit 1
fi
[ "$actual" = "$expected" ] || { printf 'checksum mismatch for %s\n' "$asset" >&2; exit 1; }

mkdir -p "$temporary/unpacked" "$INSTALL_DIR"
INSTALL_DIR=$(cd "$INSTALL_DIR" && pwd -P)
tar -xzf "$temporary/$asset" -C "$temporary/unpacked"
[ -f "$temporary/unpacked/firecrab" ] \
    || { printf '%s\n' 'release archive does not contain firecrab' >&2; exit 1; }
[ ! -d "$INSTALL_DIR/firecrab" ] \
    || { printf '%s\n' 'install destination firecrab is a directory' >&2; exit 1; }
temporary_binary=$(mktemp "$INSTALL_DIR/.firecrab-install.XXXXXX") \
    || { printf '%s\n' 'could not create a private temporary install file' >&2; exit 1; }
cp "$temporary/unpacked/firecrab" "$temporary_binary"
chmod 0755 "$temporary_binary"
mv -f "$temporary_binary" "$INSTALL_DIR/firecrab"
temporary_binary=

printf 'installed firecrab to %s/firecrab\n' "$INSTALL_DIR"
case ":${PATH:-}:" in
    *:"$INSTALL_DIR":*) ;;
    *)
        printf '%s\n' "$INSTALL_DIR is not on PATH. Add this line to your shell profile:"
        printf '  export PATH="%s:%s"\n' "$INSTALL_DIR" "\$PATH"
        ;;
esac
