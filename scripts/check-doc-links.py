#!/usr/bin/env python3
"""Validate the public English documentation."""

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PUBLIC_DOCS = REPO / "public-docs"
MAX_LINES = 170

LINK_RE = re.compile(r"\]\(\s*<?([^)>\s]+)>?(?:\s+\"[^\"]*\")?\s*\)")
PUBLIC_PATH_RE = re.compile(r"public-docs/[\w./-]+\.(?:md|py|sh)")
LEGACY_PATH_RE = re.compile(r"(?<!public-)docs/[\w./-]+\.(?:md|py|sh)")
KOREAN_RE = re.compile(r"[가-힣]")

SKIP_PREFIXES = ("http://", "https://", "mailto:", "#", "data:", "tel:")
IGNORED_PARTS = {"node_modules", "target", ".git", "__pycache__"}

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


def is_ignored(path: Path) -> bool:
    return any(part in IGNORED_PARTS for part in path.parts)


def check_public_docs() -> list[str]:
    problems: list[str] = []
    root = PUBLIC_DOCS.resolve()

    for doc in sorted(PUBLIC_DOCS.rglob("*.md")):
        relative = doc.relative_to(REPO)

        if doc.is_symlink():
            target = doc.resolve()
            if not target.is_file():
                problems.append(f"{relative}: broken symbolic link")
            elif not target.is_relative_to(root):
                problems.append(f"{relative}: symbolic link leaves public-docs")
            continue

        text = doc.read_text(encoding="utf-8")
        lines = text.splitlines()

        if len(lines) > MAX_LINES:
            problems.append(f"{relative}: {len(lines)} lines exceeds {MAX_LINES}")

        for line_number, line in enumerate(lines, start=1):
            if KOREAN_RE.search(line):
                problems.append(f"{relative}:{line_number}: Korean text is not public documentation")

        in_fenced_code = False
        for line_number, line in enumerate(lines, start=1):
            if re.match(r"^\s*(`{3,}|~{3,})", line):
                in_fenced_code = not in_fenced_code
                continue
            if in_fenced_code:
                continue

            for target in LINK_RE.findall(line):
                if target.startswith(SKIP_PREFIXES):
                    continue
                path_part = target.split("#", 1)[0]
                if not path_part:
                    continue
                if not (doc.parent / path_part).resolve().exists():
                    problems.append(f"{relative}:{line_number}: broken link {target}")

    return problems


def check_source_references() -> list[str]:
    problems: list[str] = []
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
                for reference in LEGACY_PATH_RE.findall(line):
                    relative = path.relative_to(REPO)
                    problems.append(f"{relative}:{line_number}: private docs reference {reference}")
                for reference in PUBLIC_PATH_RE.findall(line):
                    if not (REPO / reference).exists():
                        relative = path.relative_to(REPO)
                        problems.append(f"{relative}:{line_number}: broken reference {reference}")

    return problems


def main() -> int:
    problems = check_public_docs() + check_source_references()

    if problems:
        print(f"Public documentation check failed with {len(problems)} problem(s).")
        for problem in problems:
            print(f"  {problem}")
        return 1

    print("Public documentation is English, linked, and at most 170 lines per file.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
