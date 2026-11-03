# Print the last message records of one CLI session: preview <agent-id> <kind> <pane-id>.
# The transcript is found through the processes Herdr runs in the pane, so messages typed in
# the terminal count too. Tool calls and their output are filtered out before leaving the VM.
# The caller defines processes(), which prints Herdr's process information for one pane.
proc=${proc:-/proc}

# A subagent thread names its parent in its first record.
subagent() {
  head -c 4096 "$1" | head -n 1 | grep -q '"parent_thread_id":"'
}

codex_rollout() {
  # Without its shared daemon, Codex keeps the rollout of each of its threads open in the
  # pane's own process. On the shared daemon nothing ties a thread to a pane, so the card
  # stays empty rather than risk showing another session.
  for pid in $pids; do
    for path in $(ls -l "$proc/$pid/fd" 2>/dev/null |
      sed -n 's|.* -> \(/.*/\.codex/sessions/.*/rollout-[^/]*\.jsonl\)$|\1|p'); do
      subagent "$path" && continue
      if [ -z "$file" ] || [ "$path" -nt "$file" ]; then file=$path; fi
    done
  done
}

claude_transcript() {
  for pid in $pids; do
    # Claude Code records the session of each running process under its PID.
    id=$(grep -o '"sessionId":"[0-9a-f-]*"' "$HOME/.claude/sessions/$pid.json" 2>/dev/null |
      head -n 1 | cut -d'"' -f4)
    [ -n "$id" ] || continue
    for path in "$HOME"/.claude/projects/*/"$id".jsonl; do
      if [ -f "$path" ]; then file=$path; fi
    done
  done
}

preview() {
  printf 'FIRECRAB_PREVIEW %s\n' "$1"
  pids=$(processes "$3" 2>/dev/null | grep -o '"pid":[0-9]*' | cut -d: -f2)
  file=
  case $2 in
    codex)
      codex_rollout
      inputs='"type":"(user_message|UserMessage)"'
      replies='"type":"(agent_message|AgentMessage)"' ;;
    claude)
      claude_transcript
      inputs='"type":"user"'
      replies='"type":"assistant"' ;;
  esac
  [ -n "$file" ] || return 0
  # Only the end is read. A read that starts inside a record leaves out the rest of that
  # record, which could be a tool call's input.
  size=$(wc -c <"$file")
  if [ $size -gt 2097152 ]; then tail -c 2097152 "$file" | sed 1d; else cat "$file"; fi |
    grep -E "$inputs|$replies" |
    # These patterns match JSON structure only, because quotes inside message text are escaped.
    # An origin other than "human" marks a notice or a turn the user did not type.
    grep -v -e '"type":"tool_result"' -e '"type":"tool_use"' -e '"type":"thinking"' \
      -e '"type":"redacted_thinking"' -e '"isMeta":true' -e '"isSidechain":true' \
      -e '"isCompactSummary":true' \
      -e '"content":"<local-command-' -e '"content":"<task-notification>' \
      -e '"origin":{"kind":"[^h]' -e '"turnOrigin":"[^h]' |
    # The last few inputs and replies, in order, so a run of progress replies keeps the input.
    awk -v inputs="$inputs" '{
      if ($0 ~ inputs) kept["i" (n++ % 6)] = NR " " substr($0, 1, 16384)
      else kept["r" (m++ % 6)] = NR " " substr($0, 1, 16384)
    } END { for (k in kept) print kept[k] }' |
    sort -n | cut -d ' ' -f 2- | cut -b 1-16384
}
