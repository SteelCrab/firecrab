#!/usr/bin/env bash
# Runs firecrab-net-helper on the same socket path firecrab-api expects by
# default (/run/firecrab/net-helper.sock), as root with the developer's primary
# group so the socket ends up root:<group> and the unprivileged API process can
# connect to it. `sudo -g <group>` alone runs as the invoking user, not root;
# `-u root` is required too.
set -euo pipefail

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
helper_binary="${repo_dir}/target/debug/firecrab-net-helper"
developer_group=$(id -gn)
developer_uid=$(id -u)

cargo build --manifest-path "${repo_dir}/Cargo.toml" -p firecrab-net-helper

exec sudo -u root -g "${developer_group}" env \
  FIRECRAB_NET_HELPER_ALLOWED_UID="${developer_uid}" \
  "${helper_binary}"
