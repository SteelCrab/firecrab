#!/bin/bash
set -Eeuo pipefail

for _ in $(seq 1 60); do
  state=$(systemctl is-system-running 2>/dev/null || true)
  case "$state" in running|degraded) break ;; esac
  sleep 1
done
case "$state" in
  running|degraded) ;;
  *) echo '[FAIL] private systemd did not become ready; inspect the instance container logs' >&2; exit 1 ;;
esac
test -c /dev/kvm && test -r /dev/kvm && test -w /dev/kvm
if ! systemctl cat firecrab-api >/dev/null 2>&1 \
  || ! test -x /usr/local/lib/firecrab/firecrab-api; then
  # Use the release's baked installer: the selected source can require payload
  # members that have not shipped yet. Verify the installer before executing it;
  # its baked tag pins the matching, independently verified host bundle.
  # shellcheck disable=SC1091
  . /opt/firecrab-bootstrap/scripts/firecrab-release.sh
  installer=$(mktemp -d)
  trap 'rm -rf "$installer"' EXIT
  for asset in install.sh SHA256SUMS; do
    curl --proto '=https' --tlsv1.2 -fsSL \
      "$(firecrab_release_asset_url latest "$asset")" -o "$installer/$asset"
  done
  if ! firecrab_verify_sha256 "$installer/SHA256SUMS" "$installer/install.sh"; then
    echo '[FAIL] private runtime release installer checksum mismatch' >&2
    exit 1
  fi
  # Its readiness check requests '/', so install the bundled baseline dashboard
  # too. --no-frontend would leave '/' at 404 and fail the first installation.
  bash "$installer/install.sh" --libc gnu
fi
systemctl start firecrab-dev-publish.service
