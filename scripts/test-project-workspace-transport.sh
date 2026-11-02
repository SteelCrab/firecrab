#!/usr/bin/env bash
# Real API/SSH/native process regression in an owned root container. No host services or VM claims.
set -euo pipefail
[[ $# == 2 ]] || { echo "Usage: $0 API_TEST_BINARY PROFILE_DIRECTORY" >&2; exit 2; }
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
[[ -f "$binary" ]] || { echo 'API test binary is missing' >&2; exit 2; }
mkdir -p "$2"
profiles=$(cd "$2" && pwd)
base_image=${FIRECRAB_GUEST_QA_BASE_IMAGE:-}
if [[ -z "$base_image" ]]; then
  [[ -r /etc/os-release ]] || { echo 'Set FIRECRAB_GUEST_QA_BASE_IMAGE for this Linux test binary' >&2; exit 2; }
  # shellcheck disable=SC1091
  base_image="ubuntu:$(. /etc/os-release && printf '%s' "$VERSION_ID")"
fi
context=$(mktemp -d "${TMPDIR:-/tmp}/firecrab-workspace-transport.XXXXXX")
name="firecrab-workspace-transport-${context##*.}"
image="firecrab-workspace-transport:${context##*.}"
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker image rm "$image" >/dev/null 2>&1 || true
  rm -rf "$context"
}
trap cleanup EXIT
# The exact production package is checked again by the real Rust archive loader.
case "$(uname -m)" in
  x86_64) arch=x64; sha=b433bf4f0227321a7014d8460d10a8f958adc0f45aa79bd889e84e65e8f88363;;
  aarch64|arm64) arch=arm64; sha=36d9c14036489b63de84ebace837fcacf7e60e669a0dc715802c5443684ea4dc;;
  *) echo 'Unsupported editor architecture' >&2; exit 2;;
esac
archive=$context/editor.tar.gz
if [[ -n ${FIRECRAB_EDITOR_PACKAGE_ARCHIVE:-} ]]; then
  cp "$FIRECRAB_EDITOR_PACKAGE_ARCHIVE" "$archive"
else
  curl --fail --location --retry 2 --max-time 180 --proto '=https' --proto-redir '=https' \
    "https://github.com/gitpod-io/openvscode-server/releases/download/openvscode-server-v1.109.5/openvscode-server-v1.109.5-linux-$arch.tar.gz" -o "$archive"
fi
python3 - "$archive" "$sha" <<'PY'
import hashlib
from pathlib import Path
import sys
path=Path(sys.argv[1])
assert path.is_file() and path.stat().st_size <= 128*1024*1024, 'Invalid editor QA archive'
assert hashlib.file_digest(path.open('rb'),'sha256').hexdigest()==sys.argv[2], 'Editor QA SHA-256 mismatch'
PY
printf 'editor.tar.gz\n' > "$context/.dockerignore"
cp -R "$repo_root/packaging/guest-console" "$context/native"
cat > "$context/driver.c" <<'C'
#include "workspace.h"
#include <string.h>
/* This executable exists only in the disposable test image. The fixed directory
 * replaces mount verification, while the real file/PTY/command code is unchanged. */
int main(int n,char **v) {
  if(n!=6) return 2;
  char *a[8]={v[0],v[1],v[2],v[3],v[4],v[5],"/workspace/shared",0};
  if(!strcmp(v[1],"--workspace-files")) return firecrab_workspace_files(7,a);
  if(!strcmp(v[1],"--workspace-terminal")) return firecrab_workspace_terminal(7,a);
  if(!strcmp(v[1],"--workspace-command")) return firecrab_workspace_command(7,a);
  return 2;
}
C
cat > "$context/Dockerfile" <<'DOCKERFILE'
ARG BASE_IMAGE=ubuntu:26.04
FROM ${BASE_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential coreutils e2fsprogs findutils gawk libgcc-s1 openssh-client openssh-server \
    passwd procps python3 tar util-linux && rm -rf /var/lib/apt/lists/*
COPY native /qa/native
COPY driver.c /qa/driver.c
RUN cc -std=c11 -Wall -Wextra -Werror -DFIRECRAB_WORKSPACE_TEST -I /qa/native \
    /qa/native/workspace.c /qa/native/workspace_process.c /qa/native/terminal.c \
    /qa/native/command.c /qa/driver.c -o /usr/local/sbin/firecrab-console-resize
DOCKERFILE
docker build --build-arg "BASE_IMAGE=$base_image" -t "$image" "$context"
docker run --rm --name "$name" --network none --cpus 2 --memory 2g \
  -v "$binary:/qa/firecrab-api-tests:ro" -v "$profiles:/profiles" \
  -v "$archive:/qa/editor.tar.gz:ro" \
  -e FIRECRAB_WORKSPACE_TRANSPORT_QA=1 -e FIRECRAB_EDITOR_PACKAGE_ARCHIVE=/qa/editor.tar.gz \
  -e 'LLVM_PROFILE_FILE=/profiles/workspace-%m-%p.profraw' \
  "$image" bash -ec '
    /qa/firecrab-api-tests workspace_transport_retains_file_receipts_ptys_skills_and_editor_instances --ignored --nocapture
    /qa/firecrab-api-tests workspace_backup_qa_restores_real_ext4_contents_and_sqlite_without_a_workload_vm --ignored --nocapture
  '
