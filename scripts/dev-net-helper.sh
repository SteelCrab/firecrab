#!/usr/bin/env bash
# Runs firecrab-helper on the same socket path firecrab-api expects by
# default (/run/firecrab/net-helper.sock), as root with the invoking user's
# primary group so the socket ends up root:<group> and the (unprivileged) API
# process can connect to it. `sudo -g <group>` alone runs as the invoking
# user, not root — `-u root` is required too.
#
# VMs run in systemd units the helper starts, and the helper only runs the
# root-owned `firecrab-api` installed beside itself. So the helper and the API
# built from this checkout are first copied into a root-owned directory; rerun
# this script after rebuilding either of them.
set -euo pipefail

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
build_dir="${repo_dir}/target/debug"
stage=/run/firecrab-dev

for binary in firecrab-helper firecrab-api; do
  if [ ! -x "${build_dir}/${binary}" ]; then
    echo "missing ${build_dir}/${binary}; run: cargo build -p firecrab-api -p firecrab-helper" >&2
    exit 1
  fi
done

sudo install -d -m 0755 -o root -g root "$stage"
sudo install -m 0755 -o root -g root \
  "${build_dir}/firecrab-helper" "${build_dir}/firecrab-api" "$stage/"

exec sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  "${stage}/firecrab-helper"
