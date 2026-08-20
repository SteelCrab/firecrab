#!/usr/bin/env bash
# Build the --bin-dir / --dashboard-dir payload used by CI installer jobs.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

cargo build --release --locked -p firecrab-api -p firecrab-net-helper -p firecrab-cli -p firecrab-bench
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
