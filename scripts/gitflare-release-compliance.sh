#!/usr/bin/env bash
# Clean-room release-compliance preflight for Gitflare / TalkPipe.
#
# The runner must provide a fresh source checkout. This script additionally
# isolates Cargo/npm working state inside the checkout, refuses a dirty source
# tree, verifies the requested commit, executes the release-compliance contracts,
# and leaves a small hash-bound receipt bundle under dist/gitflare-receipts/.
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

EXPECTED_SHA=${GITFLARE_EXPECTED_SHA:-}
ACTUAL_SHA=$(git rev-parse HEAD)
WORK="$ROOT/.gitflare/release-compliance-preflight"
RECEIPTS="$ROOT/dist/gitflare-receipts"
LOG="$RECEIPTS/run.log"

fail() {
    printf 'preflight: FAIL: %s\n' "$*" >&2
    return 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

if [ -n "$EXPECTED_SHA" ]; then
    case "$EXPECTED_SHA" in
        *[!0-9a-fA-F]*|'') fail "GITFLARE_EXPECTED_SHA must be hexadecimal" ;;
    esac
    [ "${#EXPECTED_SHA}" -eq 40 ] || [ "${#EXPECTED_SHA}" -eq 64 ] \
        || fail "GITFLARE_EXPECTED_SHA must be a 40- or 64-character object id"
    [ "${ACTUAL_SHA,,}" = "${EXPECTED_SHA,,}" ] \
        || fail "checkout SHA mismatch: expected $EXPECTED_SHA, got $ACTUAL_SHA"
fi

# Assert the source boundary before this script creates any untracked state.
if [ -n "$(git status --porcelain=v1 --untracked-files=all)" ]; then
    git status --short
    fail "source checkout is not clean"
fi

git fsck --no-reflogs --connectivity-only

# Fail before installing the receipt trap if the runner image itself is missing
# a required tool. The runner log is the receipt for an invalid execution image.
for command in git python3 cargo rustc node npm shellcheck sha256sum tar zstd; do
    require_command "$command"
done

rm -rf -- "$WORK" "$RECEIPTS"
mkdir -p "$WORK/cargo" "$WORK/npm-cache" "$WORK/target" "$RECEIPTS"

# Preserve the runner's original stdout/stderr. The tee is closed before the
# evidence files are hashed so run.log cannot change underneath SHA256SUMS.
exec 3>&1 4>&2
exec > >(tee "$LOG") 2>&1

finish() {
    rc=$?
    trap - EXIT

    python3 - "$RECEIPTS/verdict.json" "$rc" "$ACTUAL_SHA" <<'PY'
import json
import sys
from datetime import datetime, timezone

path, rc, sha = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with open(path, "w", encoding="utf-8") as stream:
    json.dump(
        {
            "schemaVersion": 1,
            "profile": "release-compliance-preflight",
            "sha": sha,
            "verdict": "PASS" if rc == 0 else "FAIL",
            "exitCode": rc,
            "finishedAt": datetime.now(timezone.utc).isoformat(),
        },
        stream,
        indent=2,
        sort_keys=True,
    )
    stream.write("\n")
PY

    if [ -d "$ROOT/dist/compliance" ]; then
        cp -f "$ROOT/dist/compliance/THIRD_PARTY_NOTICES.txt" \
            "$RECEIPTS/THIRD_PARTY_NOTICES.txt" 2>/dev/null || true
        cp -f "$ROOT/dist/compliance/release-license-inventory.json" \
            "$RECEIPTS/release-license-inventory.json" 2>/dev/null || true
    fi

    # Stop writing to run.log before hashing it.
    exec 1>&3 2>&4
    exec 3>&- 4>&-

    (
        cd "$RECEIPTS"
        find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%P\n' \
            | LC_ALL=C sort \
            | while IFS= read -r file; do sha256sum "$file"; done \
            > SHA256SUMS
    )
    tar -C "$ROOT/dist" -czf "$ROOT/dist/gitflare-receipts.tar.gz" gitflare-receipts
    receipt_sha=$(sha256sum "$ROOT/dist/gitflare-receipts.tar.gz" | awk '{print $1}')
    printf 'gitflare receipt sha256: %s\n' "$receipt_sha"
    exit "$rc"
}
trap finish EXIT

export CARGO_HOME="$WORK/cargo"
export CARGO_TARGET_DIR="$WORK/target"
export npm_config_cache="$WORK/npm-cache"
export PYTHONDONTWRITEBYTECODE=1
export PYTHONOPTIMIZE=1

python3 - "$RECEIPTS/preflight.json" "$ACTUAL_SHA" <<'PY'
import hashlib
import json
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

path, sha = sys.argv[1:3]
manifest = Path("packaging/m2images.json").read_bytes()

def version(*argv):
    return subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout.strip()

record = {
    "schemaVersion": 1,
    "profile": "release-compliance-preflight",
    "startedAt": datetime.now(timezone.utc).isoformat(),
    "sha": sha,
    "platform": platform.platform(),
    "architecture": platform.machine(),
    "toolchain": {
        "python": version("python3", "--version"),
        "cargo": version("cargo", "--version"),
        "rustc": version("rustc", "--version"),
        "node": version("node", "--version"),
        "npm": version("npm", "--version"),
        "shellcheck": version("shellcheck", "--version").splitlines()[1].strip(),
    },
    "m2imageManifestSha256": hashlib.sha256(manifest).hexdigest(),
    "cachePolicy": "fresh-per-run",
}
with open(path, "w", encoding="utf-8") as stream:
    json.dump(record, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY

printf '\n== compliance unit contracts (Python optimization enabled) ==\n'
python3 -m unittest \
    scripts/test_release_compliance.py \
    scripts/test_m2image_sbom.py \
    scripts/test_m2image_compliance.py \
    scripts/test_m2image_source_publication.py \
    scripts/test_fetch_m2image_sources.py

printf '\n== shell and M2Image publication contracts ==\n'
shellcheck \
    scripts/collect-m2image-compliance.sh \
    scripts/package-m2images.sh \
    scripts/package-m2image-sources.sh \
    scripts/publish-m2images-r2.sh \
    scripts/test-m2image-package-compliance.sh \
    scripts/test-m2image-source-package.sh \
    scripts/test-m2image-source-publish.sh \
    scripts/gitflare-release-compliance.sh
bash scripts/test-m2image-package-compliance.sh
bash scripts/test-m2image-source-package.sh
bash scripts/test-m2image-source-publish.sh
bash scripts/test-host-release-compliance.sh

printf '\n== fresh dependency graph ==\n'
rm -rf -- firecrab-frontend/node_modules "$ROOT/dist/compliance"
cargo fetch --locked
npm ci --omit=dev --prefix firecrab-frontend

targets=(
    x86_64-unknown-linux-musl
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-musl
    aarch64-unknown-linux-gnu
)
metadata_args=()
for target in "${targets[@]}"; do
    path="$WORK/cargo-metadata-${target}.json"
    cargo metadata --format-version=1 --locked --filter-platform "$target" > "$path"
    metadata_args+=(--cargo-metadata "$path")
done

mkdir -p dist/compliance
python3 scripts/release_compliance.py \
    "${metadata_args[@]}" \
    --frontend-lock firecrab-frontend/package-lock.json \
    --frontend-root firecrab-frontend \
    --notices-out dist/compliance/THIRD_PARTY_NOTICES.txt \
    --inventory-out dist/compliance/release-license-inventory.json \
    --deny-incompatible

printf '\n== receipt summary ==\n'
python3 - <<'PY'
import json
from pathlib import Path
inventory = json.loads(Path("dist/compliance/release-license-inventory.json").read_text())
print(f"runtime dependencies: {len(inventory['runtime'])}")
print(f"build/test-only dependencies: {len(inventory['buildTestOnly'])}")
PY

# The successful Workflow snapshot should retain evidence, not a dependency
# cache. All reusable dependency state is intentionally discarded here.
rm -rf -- "$WORK" firecrab-frontend/node_modules
printf 'preflight: PASS for %s\n' "$ACTUAL_SHA"
