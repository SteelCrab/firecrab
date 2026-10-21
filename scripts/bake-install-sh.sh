#!/usr/bin/env bash
# Produce a standalone install.sh for a GitHub Release: inline helpers + bake tag.
set -euo pipefail

[ $# -eq 2 ] || { printf 'Usage: %s <tag> <output>\n' "$0" >&2; exit 2; }
tag=$1
output=$2

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

python3 - "$root" "$tag" "$output" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
tag = sys.argv[2]
out = Path(sys.argv[3])

install = (root / "install.sh").read_text(encoding="utf-8")
helpers = (root / "scripts/firecrab-release.sh").read_text(encoding="utf-8")
body = "\n".join(
    line for line in helpers.splitlines() if not line.startswith("#!")
).rstrip() + "\n"

start = install.index("# BEGIN RELEASE_HELPERS")
end = install.index("# END RELEASE_HELPERS") + len("# END RELEASE_HELPERS")
baked = (
    install[:start]
    + "# BEGIN RELEASE_HELPERS\n"
    + body
    + "# END RELEASE_HELPERS"
    + install[end:]
)
baked = baked.replace("@FIRECRAB_RELEASE_TAG@", tag)
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(baked, encoding="utf-8")
out.chmod(0o755)
print(out)
PY
