#!/bin/bash
# stdin is the already verified, standalone release installer.
set -Eeuo pipefail
tag=${1:?release tag is required}
digest=${2:?installer SHA-256 is required}
root=/var/lib/firecrab/dev
units=/etc/systemd/system
install -d -m 0700 "$root"
exec 9>"$root/deploy.lock"
if ! flock -n 9; then
  echo 'another service dev build/deployment is in progress; cancel it before updating' >&2
  exit 1
fi

stage=$(mktemp -d "$root/update.XXXXXX")
changed=0
# Invoked by the EXIT trap.
# shellcheck disable=SC2317,SC2329
finish() {
  local status=$?
  trap - EXIT
  set +e
  if [ "$status" -ne 0 ] && [ "$changed" -eq 1 ]; then
    echo '[ROLLBACK] restoring development service settings' >&2
    for unit in firecrab-api firecrab-helper firecrab-net-helper; do
      if [ -f "$stage/$unit.conf" ]; then
        install -d "$units/$unit.service.d"
        cp "$stage/$unit.conf" "$units/$unit.service.d/90-firecrab-dev.conf"
      fi
    done
    systemctl daemon-reload || true
    systemctl restart "$helper" firecrab-api || true
    echo "[PRESERVE] update diagnostics: $stage" >&2
  else
    rm -rf -- "$stage"
  fi
  exit "$status"
}
trap finish EXIT
cat >"$stage/install.sh"
printf '%s  %s\n' "$digest" "$stage/install.sh" | sha256sum -c -
helper=firecrab-helper
if [ "$(systemctl show --property=LoadState --value firecrab-helper.service)" = not-found ]; then
  helper=firecrab-net-helper
fi
for unit in firecrab-api firecrab-helper firecrab-net-helper; do
  override="$units/$unit.service.d/90-firecrab-dev.conf"
  if [ -f "$override" ]; then cp "$override" "$stage/$unit.conf"; fi
done
changed=1
for unit in firecrab-api firecrab-helper firecrab-net-helper; do
  rm -f "$units/$unit.service.d/90-firecrab-dev.conf"
done
# Keep the running API until the installer has downloaded and verified the new
# release. Never start the older installed API against development-era data.
echo "[UPDATE] installing $tag API, CLI, helper and dashboard"
bash "$stage/install.sh" --version "$tag"
systemctl daemon-reload
systemctl restart firecrab-helper firecrab-api
for _ in $(seq 1 60); do
  if systemctl is-active --quiet firecrab-helper && systemctl is-active --quiet firecrab-api \
    && curl -fs --max-time 2 http://127.0.0.1:5523/api/info \
      | jq -e --arg version "${tag#v}" '.version == $version' >/dev/null; then
    changed=0
    echo "[PASS] managed guest: $tag is running"
    exit 0
  fi
  sleep 1
done
journalctl --no-pager -n 30 -u firecrab-api -u firecrab-helper >&2
echo 'updated API did not become ready' >&2
exit 1
