#!/usr/bin/env bash
# Execute the root-only guest contract in a disposable container, never on the API host.
set -euo pipefail

if [[ $# != 2 ]]; then
  echo "Usage: $0 INSTRUMENTED_API_TEST_BINARY PROFILE_DIRECTORY" >&2
  exit 2
fi
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
guest_helper=${FIRECRAB_GUEST_CONSOLE_BIN:-$repo_root/target/ingress/firecrab-guest-console}
[[ -f "$guest_helper" ]] || { echo "Build the trusted static guest helper with scripts/build-guest-console.sh first" >&2; exit 2; }
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
FROM busybox@sha256:8d7b1636e974e0adfd8d945955fca609304f0a56c18799dfd032d6e661382d84 AS toolbox
FROM ${BASE_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential coreutils findutils gawk libgcc-s1 \
    openssh-client openssh-server passwd procps python3 tar util-linux \
    && rm -rf /var/lib/apt/lists/*
# The distribution's busybox-static may omit account applets. Use the exact
# multi-architecture toolbox from the prepared distroless recipe instead.
COPY --from=toolbox /bin/busybox /etc/firecrab/busybox
RUN /etc/firecrab/busybox --list | grep -x adduser
DOCKERFILE
docker build --build-arg "BASE_IMAGE=$base_image" -t "$image" "$context"
docker run --rm --name "$name" --network none --cpus 2 --memory 1g \
  -v "$binary:/qa/firecrab-api-tests:ro" -v "$profiles:/profiles" \
  -v "$repo_root:/qa/source:ro" \
  -v "$guest_helper:/usr/local/sbin/firecrab-console-resize:ro" \
  -e FIRECRAB_SANDBOX_GUEST_QA=1 -e 'LLVM_PROFILE_FILE=/profiles/guest-%m-%p.profraw' \
  "$image" /bin/sh -eu -c '
    /qa/firecrab-api-tests ai_sandbox::guest::tests::guest_ --ignored --nocapture
    # The prepared-guest path must work without host account applet links.
    for name in addgroup delgroup; do
      if [ -e "/usr/sbin/$name" ]; then mv "/usr/sbin/$name" "/tmp/hidden-$name"; fi
    done
    FIRECRAB_SANDBOX_BUSYBOX_ACCOUNTS=1 python3 /qa/source/scripts/test-sandbox-guest-shell.py
    # Alpine exposes the applets too: adduser -G can invoke addgroup by name
    # and record the user explicitly in its own primary group member list.
    for name in adduser deluser addgroup delgroup; do
      ln -s /etc/firecrab/busybox "/usr/local/sbin/$name"
    done
    FIRECRAB_SANDBOX_BUSYBOX_ACCOUNTS=1 FIRECRAB_SANDBOX_BUSYBOX_VISIBLE_APPLETS=1 \
      python3 /qa/source/scripts/test-sandbox-guest-shell.py
  '
