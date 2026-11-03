set -eu
export GH_PROMPT_DISABLED=1
# Newest first, 100 per page, at most 10 pages (1,000 repositories). `tojson`
# prints each repository as one raw line whatever gh's output formatting.
# No page starts after 15 seconds and every page ends by 25 seconds, so the
# list arrives before the dashboard stops waiting for it.
started=$(date +%s)
page=1
while :; do
  rows=$(timeout $((started + 25 - $(date +%s))) gh api \
    "user/repos?sort=updated&per_page=100&page=$page&affiliation=owner,collaborator,organization_member" \
    --jq '.[] | {name: .full_name, private: .private, archived: .archived} | tojson') || {
    status=$?
    # A later page that fails (rate limit, slow API) keeps the newest pages.
    [ "$page" -gt 1 ] || exit "$status"
    break
  }
  [ -n "$rows" ] || break
  printf '%s\n' "$rows"
  [ "$page" -lt 10 ] && [ "$(printf '%s\n' "$rows" | grep -c '')" -ge 100 ] &&
    [ $(($(date +%s) - started)) -lt 15 ] || break
  page=$((page + 1))
done
