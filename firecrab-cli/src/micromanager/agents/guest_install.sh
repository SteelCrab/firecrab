set -eu
export PATH=/root/.local/bin:/usr/local/bin:/usr/bin:/bin:/sbin
umask 077
lock=/tmp/firecrab-agent-prepare.lock
mkdir "$lock" 2>/dev/null || { echo "Another environment preparation is running in this MicroVM." >&2; exit 1; }
work=""
trap 'if [ -n "$work" ]; then rm -rf "$work"; fi; rmdir "$lock" 2>/dev/null || :' EXIT
work=$(mktemp -d /tmp/firecrab-agent-install.XXXXXX)
printf 'FIRECRAB_STEP=1\n'
if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends ca-certificates curl bash git gh coreutils libstdc++6
elif command -v apk >/dev/null 2>&1; then
  apk add --no-cache ca-certificates curl bash git github-cli coreutils libstdc++
else
  echo "Use a Debian, Ubuntu or Alpine MicroVM for automatic CLI installation." >&2
  exit 1
fi
printf 'FIRECRAB_STEP=2\n'
if [ "$1" != github ] && { ! command -v herdr >/dev/null 2>&1 || ! herdr terminal session control --help >/dev/null 2>&1; }; then
  curl --connect-timeout 15 --retry 2 -fsSL https://herdr.dev/install.sh -o "$work/herdr.sh"
  sh "$work/herdr.sh"
fi
printf 'FIRECRAB_STEP=3\n'
case "$1" in
  codex)
    if ! command -v codex >/dev/null 2>&1; then
      curl --connect-timeout 15 --retry 2 -fsSL https://chatgpt.com/codex/install.sh -o "$work/codex.sh"
      sh "$work/codex.sh"
    fi ;;
  claude)
    if ! command -v claude >/dev/null 2>&1; then
      curl --connect-timeout 15 --retry 2 -fsSL https://claude.ai/install.sh -o "$work/claude.sh"
      bash "$work/claude.sh" stable
    fi ;;
  github) : ;;
  *) echo "Select GitHub, Codex or Claude Code." >&2; exit 1 ;;
esac
printf 'FIRECRAB_STEP=4\n'
if [ "$1" = github ]; then command -v gh >/dev/null; else
  command -v herdr >/dev/null
  command -v "$1" >/dev/null
fi
mkdir -p /workspace/firecrab
printf 'Environment is ready. Connect the CLI account in this MicroVM.\n'
