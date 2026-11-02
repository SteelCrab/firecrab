#!/usr/bin/env bash
# Run real OCI filesystem and provisioning tests with the host tools they need.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if [[ $(uname -s) == Darwin ]]; then
    if ! command -v brew >/dev/null 2>&1; then
        echo 'OCI tests require Homebrew: brew install coreutils e2fsprogs fakeroot' >&2
        exit 1
    fi
    coreutils_prefix=$(brew --prefix coreutils)
    e2fsprogs_prefix=$(brew --prefix e2fsprogs)
    if [[ ! -x "$coreutils_prefix/libexec/gnubin/chown" ]]; then
        echo 'OCI tests require GNU chown: brew install coreutils' >&2
        exit 1
    fi
    # macOS-protected chown cannot load fakeroot's ownership interposer.
    export PATH="$coreutils_prefix/libexec/gnubin:$e2fsprogs_prefix/sbin:$PATH"
fi

for tool in cargo fakeroot chown mkfs.ext4 tune2fs debugfs; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Missing OCI test tool: $tool (install coreutils, e2fsprogs and fakeroot)" >&2
        exit 1
    fi
done

if [[ $(id -u) == 0 ]]; then
    echo 'Run OCI tests as a non-root user so ownership conversion is exercised.' >&2
    exit 1
fi

cd -- "$repo_root"
exec cargo test -p firecrab-api --bin firecrab-api oci -- "$@"
