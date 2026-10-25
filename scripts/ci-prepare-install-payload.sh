#!/usr/bin/env bash
# Build the --bin-dir / --dashboard-dir payload used by CI installer jobs.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

(
    if [[ ${FIRECRAB_QA_COVERAGE:-0} == 1 ]]; then
        # shellcheck source=/dev/null
        source <(cargo llvm-cov show-env --sh)
        # Keep counters after systemd stops/restarts the API with SIGTERM.
        export RUSTFLAGS="${RUSTFLAGS:-} -C llvm-args=-runtime-counter-relocation"
        cargo llvm-cov clean --workspace
    fi
    cargo build --release --locked -p firecrab-api -p firecrab-helper -p firecrab-cli
)
./scripts/build-ingress.sh target/release
./scripts/build-guest-console.sh target/release
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend

# Prepare the same target-complete attribution payload installed from releases.
cargo fetch --locked
cargo fetch --manifest-path firecrab-frontend/Cargo.toml --locked
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
cargo metadata --manifest-path firecrab-frontend/Cargo.toml \
    --format-version=1 --locked --filter-platform wasm32-unknown-unknown > "$tmp/cargo-metadata-wasm.json"
metadata_args+=(--cargo-metadata "$tmp/cargo-metadata-wasm.json")
python3 scripts/release_compliance.py \
    "${metadata_args[@]}" \
    --frontend-lock firecrab-frontend/package-lock.json \
    --frontend-root firecrab-frontend \
    --notices-out dist/compliance/THIRD_PARTY_NOTICES.txt \
    --inventory-out dist/compliance/release-license-inventory.json \
    --deny-incompatible
