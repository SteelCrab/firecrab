#!/usr/bin/env python3
"""Validate CHANGELOG.md and extract a tagged release's notes."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CHANGELOG = REPO / "CHANGELOG.md"
CARGO_TOML = REPO / "Cargo.toml"

REQUIRED_SECTIONS = ("Added", "Changed", "Deprecated", "Fixed", "Improved")
HEADING_RE = re.compile(r"^## \[(\d+\.\d+\.\d+)\] - (\d{4}-\d{2}-\d{2})\s*$")
WORKSPACE_VERSION_RE = re.compile(
    r"^\[workspace\.package\]\s*$|^version\s*=\s*\"(\d+\.\d+\.\d+)\"\s*$",
    re.MULTILINE,
)
VERSION_LINE_RE = re.compile(r"^version\s*=\s*\"(\d+\.\d+\.\d+)\"\s*$")


class ChangelogError(Exception):
    """The changelog cannot satisfy the requested operation."""


def workspace_version(cargo_toml: Path) -> str:
    text = cargo_toml.read_text(encoding="utf-8")
    in_workspace_package = False
    for line in text.splitlines():
        if line.strip() == "[workspace.package]":
            in_workspace_package = True
            continue
        if in_workspace_package:
            if line.startswith("["):
                break
            match = VERSION_LINE_RE.match(line)
            if match:
                return match.group(1)
    raise ChangelogError(f"{cargo_toml}: no [workspace.package] version")


def _split_releases(text: str) -> list[tuple[str, str, str]]:
    """Return (version, date, full_block) for each ## [x.y.z] heading."""
    lines = text.splitlines()
    starts: list[tuple[int, str, str]] = []
    for index, line in enumerate(lines):
        match = HEADING_RE.match(line)
        if match:
            starts.append((index, match.group(1), match.group(2)))

    releases: list[tuple[str, str, str]] = []
    for offset, (index, version, date) in enumerate(starts):
        end = starts[offset + 1][0] if offset + 1 < len(starts) else len(lines)
        block = "\n".join(lines[index:end]).rstrip() + "\n"
        releases.append((version, date, block))
    return releases


def validate_text(text: str, expected_version: str) -> list[str]:
    problems: list[str] = []
    if not text.lstrip().startswith("# Changelog"):
        problems.append("CHANGELOG.md must start with '# Changelog'")

    releases = _split_releases(text)
    if not releases:
        problems.append(
            "CHANGELOG.md needs a version heading like '## [0.1.0] - YYYY-MM-DD'"
        )
        return problems

    latest_version, _date, block = releases[0]
    if latest_version != expected_version:
        problems.append(
            f"latest heading is [{latest_version}], expected workspace {expected_version}"
        )

    headings = [
        line[4:]
        for line in block.splitlines()
        if line.startswith("### ")
    ]
    if tuple(headings) != REQUIRED_SECTIONS:
        problems.append(
            "release "
            f"{latest_version} must have sections in order: "
            + ", ".join(REQUIRED_SECTIONS)
            + f" (found {', '.join(headings) or 'none'})"
        )

    for section in REQUIRED_SECTIONS:
        marker = f"### {section}"
        if marker not in block:
            problems.append(f"missing ### {section}")
            continue
        after = block.split(marker, 1)[1]
        next_heading = after.find("\n### ")
        body = after if next_heading < 0 else after[:next_heading]
        if not re.search(r"(?m)^\s*[-*]\s+\S", body):
            problems.append(f"### {section} needs at least one list item")

    return problems


def validate_path(path: Path, expected_version: str) -> list[str]:
    if not path.is_file():
        return [f"{path}: missing CHANGELOG.md"]
    return validate_text(path.read_text(encoding="utf-8"), expected_version)


def extract_notes(text: str, tag: str) -> str:
    version = tag[1:] if tag.startswith("v") else tag
    for found, _date, block in _split_releases(text):
        if found == version:
            return block
    raise ChangelogError(f"CHANGELOG.md has no heading for {version}")


def previous_version(text: str, tag: str) -> str | None:
    """Return the tag of the release listed directly below `tag`.

    Release notes credit whoever worked on that release, so the contributor
    range starts here. `None` means `tag` is the oldest release documented,
    which has no predecessor to compare against.
    """
    version = tag[1:] if tag.startswith("v") else tag
    releases = _split_releases(text)
    for index, (found, _date, _block) in enumerate(releases):
        if found != version:
            continue
        if index + 1 >= len(releases):
            return None
        return f"v{releases[index + 1][0]}"
    raise ChangelogError(f"CHANGELOG.md has no heading for {version}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--extract",
        metavar="TAG",
        help="print the CHANGELOG section for a git tag (e.g. v0.1.0)",
    )
    parser.add_argument(
        "--previous",
        metavar="TAG",
        help="print the tag released before TAG; prints nothing for the oldest",
    )
    args = parser.parse_args(argv)

    try:
        version = workspace_version(CARGO_TOML)
    except ChangelogError as error:
        print(error)
        return 1

    if args.extract or args.previous:
        if not CHANGELOG.is_file():
            print(f"{CHANGELOG}: missing CHANGELOG.md")
            return 1
        changelog_text = CHANGELOG.read_text(encoding="utf-8")
        try:
            if args.extract:
                sys.stdout.write(extract_notes(changelog_text, args.extract))
            else:
                earlier = previous_version(changelog_text, args.previous)
                if earlier:
                    print(earlier)
        except ChangelogError as error:
            print(error)
            return 1
        return 0

    problems = validate_path(CHANGELOG, version)
    if problems:
        print(f"Changelog check failed with {len(problems)} problem(s).")
        for problem in problems:
            print(f"  {problem}")
        return 1

    print(
        f"CHANGELOG.md has [{version}] with "
        + ", ".join(REQUIRED_SECTIONS)
        + "."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
