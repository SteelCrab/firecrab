set -eu
umask 022
stage=$(mktemp -d /tmp/firecrab-skills.XXXXXX)
trap 'rm -rf "$stage"' EXIT
tar -x -C "$stage"
for dir in /root/.agents/skills /root/.claude/skills /root/.gemini/skills; do
  mkdir -p "$dir"
  for existing in "$dir"/*; do
    [ -f "$existing/.firecrab-skill" ] || continue
    [ -d "$stage/${existing##*/}" ] || rm -rf "$existing"
  done
  for skill in "$stage"/*; do
    [ -d "$skill" ] || continue
    name=${skill##*/}
    if [ -e "$dir/$name" ] && [ ! -f "$dir/$name/.firecrab-skill" ]; then
      printf 'conflict:%s/%s\n' "$dir" "$name"
      continue
    fi
    rm -rf "$dir/$name"
    cp -R "$skill" "$dir/$name"
  done
done
mkdir -p /root/.local/share/firecrab
printf '%s' "$1" | base64 -d > /root/.local/share/firecrab/skills.json
