#!/usr/bin/env python3
"""Build GitHub Release notes: install URL, binaries, changelog, contributor icons."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

HOST_BUNDLES = (
    ("firecrab-host-x86_64-gnu.tar.gz", "Linux x86_64, glibc (Debian, Fedora, Arch, openSUSE, Ubuntu)"),
    ("firecrab-host-x86_64-musl.tar.gz", "Linux x86_64, musl (Alpine; static)"),
    ("firecrab-host-aarch64-gnu.tar.gz", "Linux aarch64 / arm64, glibc"),
    ("firecrab-host-aarch64-musl.tar.gz", "Linux aarch64 / arm64, musl (static)"),
)

RAW_TARBALLS = (
    ("firecrab-x86_64-unknown-linux-gnu.tar.gz", "x86_64 glibc API + helper only"),
    ("firecrab-x86_64-unknown-linux-musl.tar.gz", "x86_64 static musl API + helper only"),
    ("firecrab-aarch64-unknown-linux-gnu.tar.gz", "aarch64 glibc API + helper only"),
    ("firecrab-aarch64-unknown-linux-musl.tar.gz", "aarch64 static musl API + helper only"),
)


def _load_changelog():
    path = Path(__file__).with_name("check-changelog.py")
    import importlib.util

    spec = importlib.util.spec_from_file_location("check_changelog", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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


def git_contributors(repo: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(repo), "shortlog", "-sn", "--all"],
        check=True,
        capture_output=True,
        text=True,
    )
    names: list[str] = []
    for line in result.stdout.splitlines():
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
    asset = f"https://github.com/{repo}/releases/download/{pinned}"

    changelog = _load_changelog()
    changelog_block = changelog.extract_notes(changelog_text, pinned)

    lines: list[str] = [
        f"# firecrab {version}",
        "",
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
        "## Binaries",
        "",
        "Host bundles include `firecrab-api`, `firecrab-net-helper`, the dashboard,",
        "systemd units, and `firecrab-doctor`. `install.sh` downloads one of these.",
        "",
        "| File | Installs on |",
        "| --- | --- |",
    ]
    for name, who in HOST_BUNDLES:
        lines.append(f"| [`{name}`]({asset}/{name}) | {who} |")
    lines.extend(
        [
            "",
            "API + helper only (no dashboard or units):",
            "",
            "| File | Contents |",
            "| --- | --- |",
        ]
    )
    for name, who in RAW_TARBALLS:
        lines.append(f"| [`{name}`]({asset}/{name}) | {who} |")
    lines.extend(
        [
            "",
            f"Checksums: [`SHA256SUMS`]({asset}/SHA256SUMS).",
            f"Standalone installer: [`install.sh`]({pinned_install}).",
            "",
            "## Changelog",
            "",
            changelog_block.rstrip(),
            "",
            "## Contributors",
            "",
        ]
    )
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

    if args.contributors_file:
        contributors = args.contributors_file.read_text(encoding="utf-8").splitlines()
    else:
        contributors = git_contributors(REPO)

    write_notes(
        tag=args.tag,
        changelog_text=args.changelog.read_text(encoding="utf-8"),
        contributors=contributors,
        repo=args.repo,
        dest=args.out,
    )
    print(args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
