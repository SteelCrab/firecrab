#!/usr/bin/env bash
# Run the actual host installer with isolated paths and fake service controls.
set -euo pipefail
[ "$#" -eq 1 ] || { printf 'Usage: %s <ingress-build-directory>\n' "$0" >&2; exit 2; }
[ "$(id -u)" = 0 ] || { printf 'run in a disposable Linux build container as root\n' >&2; exit 1; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
engine=$(cd -- "$1" && pwd)
scratch=$(mktemp -d "$root/target/ingress-install.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/stubs" "$scratch/config/pipe" "$scratch/units"
# check_systemd only checks this directory. No real systemd jobs are submitted.
mkdir -p /run/systemd/system
for name in firecrab-api firecrab-helper firecrab; do
    printf '#!/bin/sh\nexit 0\n' >"$scratch/bin/$name"
    chmod +x "$scratch/bin/$name"
done
cp "$engine/firecrab-ingress" "$scratch/bin/firecrab-ingress"
cp -R "$engine/ingress" "$scratch/bin/ingress"
for name in ip nft dnsmasq dhcp_release fakeroot curl zstd firecracker; do
    printf '#!/bin/sh\nexit 0\n' >"$scratch/stubs/$name"
    chmod +x "$scratch/stubs/$name"
done
cat >"$scratch/stubs/systemctl" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >>"$INGRESS_TEST_CALLS"
exit 0
SH
chmod +x "$scratch/stubs/systemctl"
printf '{"routes":[{"host":"keep.example.com"}]}\n' >"$scratch/config/pipe/manifest.json"
cp "$scratch/config/pipe/manifest.json" "$scratch/original.json"
for _attempt in 1 2; do
    env PATH="$scratch/stubs:$PATH" INGRESS_TEST_CALLS="$scratch/calls" \
        PREFIX="$scratch/installed" DATADIR="$scratch/data" \
        CONFDIR="$scratch/config" UNITDIR="$scratch/units" \
        FIRECRAB_USER=root FIRECRAB_GROUP=root \
        bash "$root/install.sh" --bin-dir "$scratch/bin" --no-deps --no-frontend \
        >"$scratch/install.log" 2>&1 || { cat "$scratch/install.log" >&2; exit 1; }
    cmp "$engine/firecrab-ingress" "$scratch/installed/lib/firecrab/firecrab-ingress"
    cmp "$engine/ingress/LICENSE.nginx" "$scratch/installed/lib/firecrab/ingress/LICENSE.nginx"
    cmp "$scratch/original.json" "$scratch/config/pipe/manifest.json"
done
if grep -q firecrab-ingress "$scratch/calls"; then
    printf 'install must not start an unconfigured ingress\n' >&2
    exit 1
fi
printf 'PASS initial install and reinstall bundle ingress and preserve saved routes\n'
