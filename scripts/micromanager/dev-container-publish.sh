#!/bin/bash
set -Eeuo pipefail

# Keep the API's loopback-only security policy. Docker publishes this relay on
# the container's private interface, and only 127.0.0.1 on the client host.
address=$(ip -4 -o address show scope global | awk 'NR == 1 { sub(/\/.*/, "", $4); print $4 }')
[[ "$address" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { echo '[FAIL] private Docker interface has no IPv4 address' >&2; exit 1; }
exec socat "TCP4-LISTEN:5523,bind=$address,reuseaddr,fork" TCP4:127.0.0.1:5523
