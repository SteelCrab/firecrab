#!/usr/bin/env python3
"""Build GitHub Release notes: install URL, changelog, contributor icons."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def _load_changelog():
    path = Path(__file__).with_name("check-changelog.py")
    import importlib.util

    spec = importlib.util.spec_from_file_location("check_changelog", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def release_name(tag: str) -> str:
    """Title shown above the release body, e.g. `firecrab v0.1.2`."""
    pinned = tag if tag.startswith("v") else f"v{tag}"
    return f"firecrab {pinned}"


def _strip_version_heading(block: str) -> str:
    """Drop the `## [x.y.z] - date` line the release page already states."""
    lines = block.splitlines()
    if lines and lines[0].startswith("## ["):
        lines = lines[1:]
    return "\n".join(lines).strip()


def _absolute_repo_links(block: str, repo: str, tag: str) -> str:
    """Point in-repo markdown links at this tag's tree.

    A release page is not a tree page, so `](public-docs/oci.md)` 404s there.
    """
    base = f"https://github.com/{repo}/blob/{tag}"
    return re.sub(
        r"\]\((?!https?://|#)([^)]+)\)",
        lambda match: f"]({base}/{match.group(1).lstrip('./')})",
        block,
    )


def _link_definitions(changelog_text: str, block: str) -> list[str]:
    """Reference definitions for the shorthand links this block uses.

    The changelog keeps them in one list at the bottom of the file, so a
    single release's block carries `[#145]` with nothing to resolve it.
    """
    defined = dict(re.findall(r"(?m)^\[([^\]]+)\]:\s*(\S+)\s*$", changelog_text))
    already = set(re.findall(r"(?m)^\[([^\]]+)\]:", block))
    used = re.findall(r"\[([^\]]+)\](?!\(|:)", block)
    out: list[str] = []
    seen: set[str] = set()
    for ref in used:
        if ref in seen or ref in already or ref not in defined:
            continue
        seen.add(ref)
        out.append(f"[{ref}]: {defined[ref]}")
    return out


def filter_contributors(names: list[str]) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for name in names:
        cleaned = name.strip()
        if not cleaned:
            continue
        lower = cleaned.lower()
        if "bot" in lower or lower.endswith("[bot]"):
            continue
        if cleaned in seen:
            continue
        seen.add(cleaned)
        out.append(cleaned)
    return out


def _run_git(argv: list[str]) -> str:
    return subprocess.run(
        argv,
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def git_contributors(
    repo: Path,
    since: str | None = None,
    runner: Callable[[list[str]], str] = _run_git,
) -> list[str]:
    """Names behind the commits in a release.

    `since` is the previous release's tag, which limits the list to work done
    for this release. Without it the whole history is counted, which credits
    every past release's contributors again.
    """
    argv = ["git", "-C", str(repo), "shortlog", "-sn"]
    argv.append(f"{since}..HEAD" if since else "--all")
    names: list[str] = []
    for line in runner(argv).splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2:
            names.append(parts[1])
    return filter_contributors(names)


def build_notes(
    *,
    tag: str,
    changelog_text: str,
    contributors: list[str],
    repo: str,
) -> str:
    version = tag[1:] if tag.startswith("v") else tag
    pinned = tag if tag.startswith("v") else f"v{tag}"
    latest_install = (
        f"https://github.com/{repo}/releases/latest/download/install.sh"
    )
    pinned_install = (
        f"https://github.com/{repo}/releases/download/{pinned}/install.sh"
    )

    changelog = _load_changelog()
    changelog_block = _strip_version_heading(
        changelog.extract_notes(changelog_text, pinned)
    )
    definitions = _link_definitions(changelog_text, changelog_block)
    changelog_block = _absolute_repo_links(changelog_block, repo, pinned)

    lines: list[str] = [
        "## Install",
        "",
        "One command on a Linux host. The script detects `x86_64` or `aarch64`",
        "and `gnu` (glibc) or `musl`, then installs that host bundle.",
        "",
        "```sh",
        f"curl -fsSL {latest_install} | bash",
        "```",
        "",
        "Pin this release:",
        "",
        "```sh",
        f"curl -fsSL {pinned_install} | bash",
        "```",
        "",
        "Force a libc when auto-detect is wrong:",
        "",
        "```sh",
        "./install.sh --libc gnu    # glibc (Debian, Fedora, Arch, openSUSE, Ubuntu)",
        "./install.sh --libc musl   # Alpine / static musl",
        "```",
        "",
        "Local binaries you already built:",
        "",
        "```sh",
        "./install.sh --bin-dir target/release --dashboard-dir firecrab-frontend/dist",
        "```",
        "",
        "## Changelog",
        "",
        changelog_block.rstrip(),
        "",
    ]
    if definitions:
        lines.extend(definitions)
        lines.append("")
    lines.extend(["## Contributors", ""])
    people = filter_contributors(contributors)
    if people:
        lines.append('<p align="left">')
        for name in people:
            lines.append(
                f'  <a href="https://github.com/{name}">'
                f'<img src="https://github.com/{name}.png?size=96" '
                f'width="48" height="48" alt="{name}"/></a>'
            )
        lines.append("</p>")
    else:
        lines.append(
            f"See the [GitHub contributors graph](https://github.com/{repo}/graphs/contributors)."
        )
    lines.append("")
    return "\n".join(lines)


def write_notes(
    *,
    tag: str,
    changelog_text: str,
    contributors: list[str],
    repo: str,
    dest: Path,
) -> None:
    dest.write_text(
        build_notes(
            tag=tag,
            changelog_text=changelog_text,
            contributors=contributors,
            repo=repo,
        ),
        encoding="utf-8",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="git tag, e.g. v0.1.0")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--repo", default="SteelCrab/firecrab")
    parser.add_argument(
        "--print-name",
        action="store_true",
        help="also print the release title on stdout",
    )
    parser.add_argument(
        "--contributors-file",
        type=Path,
        help="one name per line; default is git shortlog",
    )
    parser.add_argument(
        "--changelog",
        type=Path,
        default=REPO / "CHANGELOG.md",
    )
    args = parser.parse_args(argv)

    changelog_text = args.changelog.read_text(encoding="utf-8")

    if args.contributors_file:
        contributors = args.contributors_file.read_text(encoding="utf-8").splitlines()
    else:
        # Only this release's work, so earlier releases' contributors are
        # not credited again.
        changelog = _load_changelog()
        contributors = git_contributors(
            REPO, since=changelog.previous_version(changelog_text, args.tag)
        )

    write_notes(
        tag=args.tag,
        changelog_text=changelog_text,
        contributors=contributors,
        repo=args.repo,
        dest=args.out,
    )
    if args.print_name:
        print(release_name(args.tag))
    print(args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
