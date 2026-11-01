set -eu
root=/root/.local/share/firecrab/github-auth
[ -f "$root/current" ] || exit 0
run=$(head -c 64 "$root/current")
case "$run" in *[!a-f0-9-]*|'') exit 1;; esac
root="$root/$run"
if [ -f "$root/exit" ]; then
  # The credential probe is authoritative. Wait for its short cache after successful login.
  if [ "$(cat "$root/exit")" = 0 ] && [ -f "$root/finished" ] && [ "$(($(date +%s) - $(cat "$root/finished")))" -lt 15 ]; then printf 'PENDING\n'; else printf 'FAILED\n'; fi
elif [ -f "$root/pid" ] && [ -f "$root/start" ] && [ -f "$root/boot" ]; then
  pid=$(cat "$root/pid")
  case "$pid" in *[!0-9]*|'') exit 1;; esac
  if [ "$(cat "$root/boot")" = "$(cat /proc/sys/kernel/random/boot_id)" ] && [ -r "/proc/$pid/stat" ] &&
    [ "$(sed 's/^.*) //' "/proc/$pid/stat" | awk '{print $20}')" = "$(cat "$root/start")" ] &&
    [ "$(sed 's/^.*) //' "/proc/$pid/stat" | awk '{print $1}')" != Z ]; then printf 'PENDING\n'; else printf 'FAILED\n'; fi
elif [ -f "$root/created" ] && [ "$(($(date +%s) - $(cat "$root/created")))" -lt 10 ]; then
  printf 'PENDING\n'
else
  printf 'FAILED\n'
fi
if [ -f "$root/log" ]; then
  printf 'CODE:'
  head -c 8192 "$root/log" | grep -oE '[A-Z0-9]{4}-[A-Z0-9]{4}' | head -n1 || true
  printf '\n'
fi
