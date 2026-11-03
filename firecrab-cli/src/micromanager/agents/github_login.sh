set -eu
umask 077
root=$1
printf '%s' "$$" > "$root/pid"
cat /proc/sys/kernel/random/boot_id > "$root/boot"
sed 's/^.*) //' "/proc/$$/stat" | awk '{print $20}' > "$root/start"
finish() { code=$?; trap - EXIT; date +%s > "$root/finished"; printf '%s' "$code" > "$root/exit"; exit "$code"; }
trap finish EXIT
trap 'exit 1' HUP INT TERM
# Closed stdin selects gh's headless device flow. No terminal Enter prompt blocks polling.
export GH_PROMPT_DISABLED=1 GH_BROWSER=/bin/true NO_COLOR=1
# `--scopes` adds to gh's default scopes. `workflow` lets git push GitHub Actions workflow files.
if [ "${2:-}" = refresh ]; then
  # Add `workflow` to the connected account. gh keeps the token's current scopes.
  timeout 900 gh auth refresh --hostname github.com --scopes workflow
else
  timeout 900 gh auth login --hostname github.com --git-protocol https --web --scopes workflow
  gh auth setup-git --hostname github.com
fi
