set -eu
umask 077
export GH_HOST=github.com GH_PROMPT_DISABLED=1
gh auth status --hostname github.com >/dev/null 2>&1 || {
  echo "Connect this MicroVM's GitHub account first." >&2
  exit 1
}
token=$(gh auth token --hostname github.com)
case "$token" in
  gho_*|ghp_*|github_pat_*) ;;
  *) echo "Repository creation requires a user OAuth token or PAT." >&2; exit 1 ;;
esac
unset token
account=$(timeout 15 gh api --hostname github.com user --jq .login)
request_dir=/root/.local/share/firecrab/github-repositories/$1
expected=$(printf '%s\n%s' "$account" "$2")
if [ -e "$request_dir" ] || [ -L "$request_dir" ]; then
  [ -d "$request_dir" ] && [ ! -L "$request_dir" ] &&
    [ -f "$request_dir/request" ] && [ ! -L "$request_dir/request" ] &&
    [ "$(cat "$request_dir/request")" = "$expected" ] || {
      echo "Repository creation request or GitHub account changed. Start a new request." >&2
      exit 1
    }
  if [ -f "$request_dir/created.json" ] && [ ! -L "$request_dir/created.json" ]; then
    cat "$request_dir/created.json"
    exit 0
  fi
  echo "The previous creation request has no confirmed result. Check GitHub and connect the repository from the list before creating another." >&2
  exit 1
fi
# Refuse to create a remote repository when its local destination is already occupied.
if [ -e /workspace/firecrab/project ] || [ -L /workspace/firecrab/project ]; then
  echo "This MicroVM already has a workspace folder. Use a new workspace for a new repository." >&2
  exit 1
fi
mkdir -p /root/.local/share/firecrab/github-repositories
mkdir "$request_dir"
printf '%s' "$expected" > "$request_dir/request"
printf '%s' "$3" | base64 -d > "$request_dir/payload.json"
# Journal before POST; an unconfirmed request must never be silently replayed.
if ! timeout 40 gh api --hostname github.com --method POST user/repos \
  -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2026-03-10' \
  --input "$request_dir/payload.json" \
  --jq '{name: .full_name, private: .private, archived: .archived}' \
  > "$request_dir/created.tmp"; then
  echo "Repository creation was not confirmed. Check GitHub before starting another creation request." >&2
  exit 1
fi
mv "$request_dir/created.tmp" "$request_dir/created.json"
cat "$request_dir/created.json"
