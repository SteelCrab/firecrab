#!/usr/bin/env bash
# Exercise the legacy-unit migration on a disposable CI host. A caller may
# keep a MicroVM running to check its PID and traffic after the migration.
set -euo pipefail
[ "${GITHUB_ACTIONS:-}" = true ] || { echo 'requires a disposable GitHub CI host' >&2; exit 1; }
[ "$(id -u)" -eq 0 ] || { echo 'run as root' >&2; exit 1; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
units=/etc/systemd/system
legacy_override="$units/firecrab-net-helper.service.d/89-qa-helper-rename.conf"
cleanup() {
    rm -f "$legacy_override"
    rmdir "$(dirname -- "$legacy_override")" 2>/dev/null || true
    systemctl daemon-reload
}
trap cleanup EXIT

# Seed a pre-rename installation with a real legacy unit, executable path,
# API dependency, and an administrator's drop-in. Existing VM units stay up.
systemctl stop firecrab-api firecrab-helper
systemctl disable firecrab-helper
rm -f "$units/firecrab-net-helper.service"
sed -e 's/firecrab-helper/firecrab-net-helper/g' -e '/^Alias=/d' \
    "$units/firecrab-helper.service" > "$units/firecrab-net-helper.service"
rm "$units/firecrab-helper.service"
sed -i 's/firecrab-helper/firecrab-net-helper/g' "$units/firecrab-api.service"
mkdir -p "$(dirname -- "$legacy_override")"
printf '[Service]\nEnvironment=FIRECRAB_QA_LEGACY_OVERRIDE=retained\n' > "$legacy_override"
systemctl daemon-reload
systemctl enable --now firecrab-net-helper
systemctl start firecrab-api
for _ in $(seq 1 60); do
    if curl -fsS --max-time 2 http://127.0.0.1:5523/api/host >/dev/null; then break; fi
    sleep 1
done
firecrab status --json | python3 -c 'import json,sys; assert json.load(sys.stdin)["netHelperService"] == "active"'
echo 'PASS helper legacy unit recognized by status'

./install.sh --no-deps --bin-dir target/release --dashboard-dir firecrab-frontend/dist
systemctl is-active --quiet firecrab-helper
systemctl is-active --quiet firecrab-net-helper
[ "$(systemctl show -p MainPID --value firecrab-helper)" = "$(systemctl show -p MainPID --value firecrab-net-helper)" ]
[ "$(readlink "$units/firecrab-net-helper.service")" = firecrab-helper.service ]
[ "$(readlink /usr/local/lib/firecrab/firecrab-net-helper)" = firecrab-helper ]
systemctl show -p Environment --value firecrab-helper | grep -q 'FIRECRAB_QA_LEGACY_OVERRIDE=retained'
firecrab status --json | python3 -c 'import json,sys; assert json.load(sys.stdin)["netHelperService"] == "active"'
echo 'PASS helper migration retained executable/unit aliases and legacy drop-in'
