#!/usr/bin/env bash
# Runs firecrab-net-helper on the same socket path firecrab-api expects by
# default (/run/firecrab/net-helper.sock), as root with primary group pista
# so the socket ends up root:pista and the (unprivileged) API process can
# connect to it. `sudo -g pista` alone runs as the invoking user, not root —
# `-u root` is required too.
set -euo pipefail

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)

exec sudo -u root -g pista FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  "${repo_dir}/target/debug/firecrab-net-helper"
