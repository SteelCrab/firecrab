#!/usr/bin/env python3
"""Verify every documentation link still resolves.

Two kinds of reference rot are possible here and neither one fails a build on
its own, so this is the only thing that catches them:

1. Relative links between docs — they break whenever a file moves, and the
   depth-sensitive `../` prefixes make that easy to get subtly wrong.
2. `docs/...` paths quoted in Rust doc comments, shell scripts, CI and the
   READMEs. Those are plain strings; the compiler never looks at them.

Exits non-zero if anything is unresolvable, so CI can gate on it.
"""
import os
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DOCS = REPO / "docs"

# `](target)`, optionally angle-bracketed, optionally followed by a title.
LINK_RE = re.compile(r"\]\(\s*<?([^)>\s]+)>?(?:\s+\"[^\"]*\")?\s*\)")
# A repo-rooted docs path mentioned anywhere in source/config.
DOCS_PATH_RE = re.compile(r"docs/[\w./-]+\.(?:md|py|sh)")

SKIP_PREFIXES = ("http://", "https://", "mailto:", "#", "data:", "tel:")

# Files that quote doc paths in comments or prose.
SOURCE_GLOBS = [
    "firecrab-api/src/**/*.rs",
    "firecrab-api-types/src/**/*.rs",
    "firecrab-helper-protocol/src/**/*.rs",
    "firecrab-net-helper/src/**/*.rs",
    "firecrab-frontend/src/**/*.ts",
    "firecrab-frontend/src/**/*.tsx",
    "scripts/**/*.sh",
    "scripts/**/*.py",
    "install.sh",
    "README*.md",
    ".github/workflows/*.yml",
]

IGNORED_PARTS = {"node_modules", "target", ".git", "__pycache__"}


def is_ignored(path: Path) -> bool:
    return any(part in IGNORED_PARTS for part in path.parts)


def check_doc_links() -> list[str]:
    """Relative links inside docs/, resolved from each file's own directory."""
    problems = []
    for doc in sorted(DOCS.rglob("*.md")):
        if is_ignored(doc):
            continue
        text = doc.read_text(encoding="utf-8")
        for line_number, line in enumerate(text.splitlines(), start=1):
            for target in LINK_RE.findall(line):
                if target.startswith(SKIP_PREFIXES):
                    continue
                path_part = target.split("#", 1)[0]
                if not path_part:
                    continue
                resolved = (doc.parent / path_part).resolve()
                if not resolved.exists():
                    where = doc.relative_to(REPO)
                    problems.append(f"{where}:{line_number}: {target}")
    return problems


def check_source_references() -> list[str]:
    """`docs/...` paths quoted from source, scripts, CI and the READMEs."""
    problems = []
    seen: set[Path] = set()
    for pattern in SOURCE_GLOBS:
        for path in sorted(REPO.glob(pattern)):
            if is_ignored(path) or path in seen:
                continue
            seen.add(path)
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            for line_number, line in enumerate(text.splitlines(), start=1):
                for reference in DOCS_PATH_RE.findall(line):
                    if not (REPO / reference).exists():
                        where = path.relative_to(REPO)
                        problems.append(f"{where}:{line_number}: {reference}")
    return problems


def main() -> int:
    doc_problems = check_doc_links()
    source_problems = check_source_references()

    for title, problems in (
        ("문서 간 링크", doc_problems),
        ("코드·스크립트의 docs 경로", source_problems),
    ):
        if problems:
            print(f"\n{title} — 깨진 참조 {len(problems)}건")
            for problem in problems:
                print(f"  {problem}")

    total = len(doc_problems) + len(source_problems)
    if total:
        print(f"\n총 {total}건이 해결되지 않습니다.")
        return 1

    print("문서 링크와 코드의 docs 경로가 모두 해결됩니다.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
