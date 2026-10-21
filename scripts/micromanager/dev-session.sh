#!/bin/bash
# Stdin stays open after the NUL-terminated script while its caller is alive.
set -eu
IFS= read -r -d '' script

# Only this deployment and its descendants receive cancellation signals.
set -m
bash -c "$script" -- "$@" </dev/null &
job=$!
set +m
cancelled=0
# Invoked by the signal traps below.
# shellcheck disable=SC2329
cancel() {
  if [ "$cancelled" -eq 0 ]; then
    cancelled=1
    echo '[CANCEL] stopping guest development command' >&2
    kill -TERM -- "-$job" 2>/dev/null || true
  fi
}
trap cancel HUP INT TERM USR1
(
  while IFS= read -r _; do :; done
  kill -USR1 "$$" 2>/dev/null || true
) <&0 &
monitor=$!

# A cancellation signal interrupts wait before the deployment has cleaned up.
while :; do
  status=0
  wait "$job" || status=$?
  kill -0 "$job" 2>/dev/null || break
done
trap '' HUP INT TERM USR1
kill "$monitor" 2>/dev/null || true
wait "$monitor" 2>/dev/null || true
if [ "$cancelled" -ne 0 ]; then exit 130; fi
exit "$status"
