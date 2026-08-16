#!/usr/bin/env bash
# firecrab PATH command. Only subcommand is doctor — execs firecrab-doctor.
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: firecrab <command> [args]

  doctor              diagnose host readiness (forwards to firecrab-doctor)
  -h, --help          this text
USAGE
}

die() { printf 'xx  %s\n' "$*" >&2; exit 1; }

# Sibling-first: $PREFIX/bin/firecrab pairs with $PREFIX/bin/firecrab-doctor;
# checkout and the host tarball ship firecrab.sh next to firecrab-doctor.sh.
resolve_doctor() {
    local self
    self=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    if [ -x "$self/firecrab-doctor" ]; then
        printf '%s\n' "$self/firecrab-doctor"
        return 0
    fi
    if [ -x "$self/firecrab-doctor.sh" ]; then
        printf '%s\n' "$self/firecrab-doctor.sh"
        return 0
    fi
    if command -v firecrab-doctor >/dev/null 2>&1; then
        command -v firecrab-doctor
        return 0
    fi
    die "missing firecrab-doctor (run from a checkout, or install first)"
}

case "${1:-}" in
    -h|--help)
        usage
        exit 0
        ;;
    "")
        usage >&2
        exit 2
        ;;
    doctor)
        shift
        exec "$(resolve_doctor)" "$@"
        ;;
    *)
        usage >&2
        printf 'unknown command: %s\n' "$1" >&2
        exit 2
        ;;
esac
