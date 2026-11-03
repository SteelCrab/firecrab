# One environment preparation per MicroVM. A VM stop or a lost SSH session skips
# the EXIT trap, so a lock from another boot or a dead owner is abandoned.
prepare_boot() { cat /proc/sys/kernel/random/boot_id 2>/dev/null || printf 'no-boot-id'; }
prepare_started() { [ -r "/proc/$1/stat" ] && sed 's/^.*) //' "/proc/$1/stat" | awk '{print $20}'; }
prepare_owner_alive() {
  if [ ! -f "$1/pid" ] || [ ! -f "$1/boot" ]; then
    # An owner records its identity right after mkdir; an older bare lock was left behind.
    modified=$(stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null) || return 1
    [ "$(($(date +%s) - modified))" -lt 15 ]
    return
  fi
  [ "$(cat "$1/boot")" = "$(prepare_boot)" ] || return 1
  owner=$(cat "$1/pid")
  case "$owner" in *[!0-9]*|'') return 1;; esac
  kill -0 "$owner" 2>/dev/null || return 1
  [ ! -f "$1/start" ] || [ "$(prepare_started "$owner")" = "$(cat "$1/start")" ]
}
prepare_unlock() { rm -f -- "$1/pid" "$1/boot" "$1/start"; rmdir "$1" 2>/dev/null || :; }
prepare_lock() {
  if ! mkdir "$1" 2>/dev/null; then
    if prepare_owner_alive "$1"; then
      echo "Another environment preparation is running in this MicroVM." >&2
      return 1
    fi
    echo "Removing an abandoned environment preparation lock." >&2
    prepare_unlock "$1"
    mkdir "$1" 2>/dev/null || { echo "Another environment preparation is running in this MicroVM." >&2; return 1; }
  fi
  printf '%s' "$$" > "$1/pid"
  prepare_boot > "$1/boot"
  prepare_started "$$" > "$1/start" || :
}
