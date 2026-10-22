#!/usr/bin/env python3
"""Refresh README contributor credits from every GitHub PR and issue."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlencode

START = "<!-- contributors:start -->"
END = "<!-- contributors:end -->"
EXCLUDED_LABELS = {"spam", "skip-contributors"}
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
LOGIN = re.compile(r"[A-Za-z0-9-]+")


@dataclass
class Contributor:
    login: str
    latest_number: int = 0
    pulls: set[int] = field(default_factory=set)
    issues: set[int] = field(default_factory=set)


def fetch_contributions(repository: str) -> list[dict]:
    result = subprocess.run(
        [
            "gh", "api", "--paginate", "--slurp",
            f"repos/{repository}/issues?state=all&per_page=100&sort=created&direction=asc",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=120,
    )
    pages = json.loads(result.stdout)
    if not isinstance(pages, list) or any(not isinstance(page, list) for page in pages):
        raise ValueError("GitHub did not return pages of issues")
    return [item for page in pages for item in page]


def collect_contributors(items: list[dict]) -> dict[int, Contributor]:
    contributors: dict[int, Contributor] = {}
    for item in items:
        user = item["user"]
        if user is None:
            continue
        login = user["login"]
        if user["type"] != "User" or login.casefold() == "ghost":
            continue
        labels = {label["name"].casefold() for label in item["labels"]}
        if labels & EXCLUDED_LABELS:
            continue
        user_id = user["id"]
        number = item["number"]
        if (
            not LOGIN.fullmatch(login)
            or type(user_id) is not int or user_id <= 0
            or type(number) is not int or number <= 0
        ):
            raise ValueError("GitHub returned an invalid contributor or issue number")
        contributor = contributors.setdefault(user_id, Contributor(login))
        if number > contributor.latest_number:
            contributor.login = login
            contributor.latest_number = number
        numbers = contributor.pulls if "pull_request" in item else contributor.issues
        numbers.add(number)
    return contributors


def contribution_links(repository: str, login: str, kind: str, numbers: set[int]) -> str:
    if not numbers:
        return "—"
    query = urlencode({"q": f"is:{kind} author:{login} -label:spam -label:skip-contributors"})
    collection = "pulls" if kind == "pr" else "issues"
    source = "pull" if kind == "pr" else "issues"
    latest = max(numbers)
    base = f"https://github.com/{repository}"
    return (
        f"[{len(numbers)}]({base}/{collection}?{query}) · "
        f"[latest #{latest}]({base}/{source}/{latest})"
    )


def render_contributors(repository: str, contributors: dict[int, Contributor]) -> str:
    lines = [
        "| Contributor | Pull requests | Issues |",
        "| :--- | :--- | :--- |",
    ]
    for user_id, person in sorted(contributors.items(), key=lambda row: row[1].login.casefold()):
        profile = f"https://github.com/{person.login}"
        avatar = f"https://avatars.githubusercontent.com/u/{user_id}?s=64"
        name = (
            f'<a href="{profile}"><img src="{avatar}" width="32" height="32" '
            f'alt="{person.login}" /></a> [@{person.login}]({profile})'
        )
        pulls = contribution_links(repository, person.login, "pr", person.pulls)
        issues = contribution_links(repository, person.login, "issue", person.issues)
        lines.append(f"| {name} | {pulls} | {issues} |")
    if not contributors:
        lines = ["No eligible PR or issue contributions yet."]
    return "\n".join(lines)


def replace_contributors(readme: str, table: str) -> str:
    lines = readme.splitlines()
    if (
        readme.count(START) != 1 or readme.count(END) != 1
        or lines.count(START) != 1 or lines.count(END) != 1
    ):
        raise ValueError("README must contain exactly one pair of contributor markers on separate lines")
    start = readme.index(START) + len(START)
    end = readme.index(END)
    if end < start:
        raise ValueError("README contributor markers are reversed")
    return readme[:start] + "\n\n" + table + "\n\n" + readme[end:]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--readme", type=Path, default=Path(__file__).resolve().parent.parent / "README.md")
    parser.add_argument("--check", action="store_true", help="report stale credits without writing")
    args = parser.parse_args(argv)
    if not args.repository or not REPOSITORY.fullmatch(args.repository):
        parser.error("--repository must be an owner/repository name")
    try:
        original = args.readme.read_text(encoding="utf-8")
        # Fail before requesting GitHub data when the editable boundary is invalid.
        replace_contributors(original, "")
        contributors = collect_contributors(fetch_contributions(args.repository))
        updated = replace_contributors(original, render_contributors(args.repository, contributors))
        if original == updated:
            print("Contributor credits are current.")
            return 0
        if args.check:
            print("Contributor credits need an update.")
            return 1
        args.readme.write_text(updated, encoding="utf-8")
        print(f"Updated credits for {len(contributors)} contributors.")
        return 0
    except subprocess.CalledProcessError as error:
        print(f"Cannot fetch contributor credits: {error.stderr.strip()}", file=sys.stderr)
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(f"Cannot update contributor credits: {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
