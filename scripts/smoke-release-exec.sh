#!/usr/bin/env bash
# Prove a release binary can exec without waiting for it to exit.
#
# firecrab-api is a long-lived HTTP server. The v0.1.0 tag run hung for the
# 6h job limit on every distro where the ELF actually started. 126/127 mean
# the loader could not run it; any other status (including timeout) means it
# exec'd.
set -u

usage() {
    printf 'Usage: %s [--seconds N] -- <command> [args...]\n' "${0##*/}" >&2
    exit 2
}

seconds=8
while [ $# -gt 0 ]; do
    case "$1" in
        --seconds)
            [ $# -ge 2 ] || usage
            seconds=$2
            shift 2
            ;;
        --)
            shift
            break
            ;;
        -*)
            usage
            ;;
        *)
            break
            ;;
    esac
done

[ $# -ge 1 ] || usage

case "$seconds" in
    ''|*[!0-9]*|0) printf 'seconds must be a positive integer\n' >&2; exit 2 ;;
esac

if ! command -v timeout >/dev/null 2>&1; then
    printf 'timeout(1) is required\n' >&2
    exit 1
fi

set +e
# -k: SIGKILL if the server ignores TERM, so this cannot sit out a 6h job.
timeout -k 2 "$seconds" "$@"
rc=$?
set -e

printf 'smoke-release-exec rc=%s\n' "$rc"

# 125: timeout(1) itself failed to start the command.
# 126: found but not executable. 127: not found / bad interpreter.
if [ "$rc" -eq 125 ] || [ "$rc" -eq 126 ] || [ "$rc" -eq 127 ]; then
    printf 'command is not executable (rc=%s)\n' "$rc" >&2
    exit 1
fi
exit 0
