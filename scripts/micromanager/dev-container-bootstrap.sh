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
  # The installed release is the baseline used by dev clean and --restore.
  # The standard installer verifies the release bundle checksums.
  # Its readiness check requests '/', so install the bundled baseline dashboard
  # too. --no-frontend would leave '/' at 404 and fail the first installation.
  bash /opt/firecrab-bootstrap/install.sh
fi
systemctl start firecrab-dev-publish.service
