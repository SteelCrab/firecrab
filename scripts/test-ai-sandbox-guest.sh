#!/usr/bin/env bash
# Execute the root-only guest contract in a disposable container, never on the API host.
set -euo pipefail

if [[ $# != 2 ]]; then
  echo "Usage: $0 INSTRUMENTED_API_TEST_BINARY PROFILE_DIRECTORY" >&2
  exit 2
fi
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[[ -f "$binary" ]] || { echo "API test binary is missing" >&2; exit 2; }
mkdir -p "$2"
profiles=$(cd "$2" && pwd)
base_image=${FIRECRAB_GUEST_QA_BASE_IMAGE:-}
if [[ -z "$base_image" ]]; then
  [[ -r /etc/os-release ]] || { echo "Set FIRECRAB_GUEST_QA_BASE_IMAGE to the binary's Linux distribution" >&2; exit 2; }
  # The test binary links the runner's libc; use the same Ubuntu release.
  # shellcheck disable=SC1091
  base_image="ubuntu:$(. /etc/os-release && printf '%s' "$VERSION_ID")"
fi
context=$(mktemp -d "${TMPDIR:-/tmp}/firecrab-sandbox-guest-qa.XXXXXX")
name="firecrab-sandbox-guest-qa-${context##*.}"
image="firecrab-sandbox-guest-qa:${context##*.}"
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker image rm "$image" >/dev/null 2>&1 || true
  rm -rf "$context"
}
trap cleanup EXIT
cat > "$context/Dockerfile" <<'DOCKERFILE'
ARG BASE_IMAGE=ubuntu:24.04
FROM ${BASE_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends \
    coreutils findutils gawk libgcc-s1 openssh-client openssh-server passwd procps tar util-linux \
    && rm -rf /var/lib/apt/lists/*
DOCKERFILE
docker build --build-arg "BASE_IMAGE=$base_image" -t "$image" "$context"
docker run --rm --name "$name" --network none --cpus 2 --memory 1g \
  -v "$binary:/qa/firecrab-api-tests:ro" -v "$profiles:/profiles" \
  -e FIRECRAB_SANDBOX_GUEST_QA=1 -e 'LLVM_PROFILE_FILE=/profiles/guest-%m-%p.profraw' \
  "$image" /qa/firecrab-api-tests \
  ai_sandbox::guest::tests::guest_ \
  --ignored --nocapture
