#!/usr/bin/env bash
# Build the --bin-dir / --dashboard-dir payload used by CI installer jobs.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

cargo build --release --locked -p firecrab-api -p firecrab-net-helper -p firecrab-cli
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend

# Prepare the same target-complete attribution payload installed from releases.
cargo fetch --locked
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
metadata_args=()
for target in \
    x86_64-unknown-linux-musl \
    x86_64-unknown-linux-gnu \
    aarch64-unknown-linux-musl \
    aarch64-unknown-linux-gnu; do
    path="$tmp/cargo-metadata-${target}.json"
    cargo metadata --format-version=1 --locked --filter-platform "$target" > "$path"
    metadata_args+=(--cargo-metadata "$path")
done
python3 scripts/release_compliance.py \
    "${metadata_args[@]}" \
    --frontend-lock firecrab-frontend/package-lock.json \
    --frontend-root firecrab-frontend \
    --notices-out dist/compliance/THIRD_PARTY_NOTICES.txt \
    --inventory-out dist/compliance/release-license-inventory.json \
    --deny-incompatible
