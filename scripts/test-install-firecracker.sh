#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fakebin="$tmp/fakebin"
mkdir -p "$fakebin" "$tmp/install-bin" "$tmp/notices"

cat >"$fakebin/uname" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' x86_64
EOF
chmod +x "$fakebin/uname"

cat >"$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
for arg in "$@"; do
    if [ "$arg" = '-w' ]; then
        printf '%s' 'https://github.com/firecracker-microvm/firecracker/releases/tag/v9.9.9'
        exit 0
    fi
done
out=
while [ $# -gt 0 ]; do
    if [ "$1" = '-o' ]; then
        out=$2
        shift 2
        continue
    fi
    shift
done
[ -n "$out" ] || { echo 'fake curl: missing -o' >&2; exit 2; }
cp "${FAKE_FIRECRACKER_ARCHIVE:?}" "$out"
EOF
chmod +x "$fakebin/curl"

make_archive() {
    local dest=$1 include_third_party=$2
    local stage="$tmp/stage"
    rm -rf "$stage"
    mkdir -p "$stage/release-v9.9.9-x86_64"
    cat >"$stage/release-v9.9.9-x86_64/firecracker-v9.9.9-x86_64" <<'EOF'
#!/usr/bin/env sh
printf '%s\n' 'Firecracker v9.9.9'
EOF
    chmod +x "$stage/release-v9.9.9-x86_64/firecracker-v9.9.9-x86_64"
    printf 'upstream license bytes\n' >"$stage/release-v9.9.9-x86_64/LICENSE"
    printf 'upstream notice bytes\n' >"$stage/release-v9.9.9-x86_64/NOTICE"
    if [ "$include_third_party" = yes ]; then
        printf 'upstream third-party bytes\n' >"$stage/release-v9.9.9-x86_64/THIRD-PARTY"
    fi
    tar -C "$stage" -czf "$dest" release-v9.9.9-x86_64
}

archive="$tmp/firecracker-good.tgz"
make_archive "$archive" yes
FAKE_FIRECRACKER_ARCHIVE="$archive" \
PATH="$fakebin:$PATH" \
INSTALL_DIR="$tmp/install-bin" \
FIRECRACKER_NOTICE_DIR="$tmp/notices" \
    bash "$ROOT/scripts/install-firecracker.sh" >/dev/null

test -x "$tmp/install-bin/firecracker"
grep -qx 'Firecracker v9.9.9' <("$tmp/install-bin/firecracker" --version)
cmp -s "$tmp/notices/LICENSE" <(printf 'upstream license bytes\n')
cmp -s "$tmp/notices/NOTICE" <(printf 'upstream notice bytes\n')
cmp -s "$tmp/notices/THIRD-PARTY" <(printf 'upstream third-party bytes\n')

rm -f "$tmp/install-bin/firecracker" "$tmp/notices/"*
bad="$tmp/firecracker-bad.tgz"
make_archive "$bad" no
if FAKE_FIRECRACKER_ARCHIVE="$bad" \
    PATH="$fakebin:$PATH" \
    INSTALL_DIR="$tmp/install-bin" \
    FIRECRACKER_NOTICE_DIR="$tmp/notices" \
    bash "$ROOT/scripts/install-firecracker.sh" >/dev/null 2>&1; then
    echo 'installer accepted a Firecracker archive without THIRD-PARTY' >&2
    exit 1
fi
test ! -e "$tmp/install-bin/firecracker"

printf 'Firecracker installer notice tests passed\n'
