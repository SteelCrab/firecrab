#!/usr/bin/env python3
"""Reports the microManager CI jobs on pull requests that touch them.

Runs in CI after the GitHub-hosted macOS and Windows microManager jobs. When a
pull request changes either platform's files, it keeps one comment on the pull
request, updated on every push, with:

- each platform job's result, steps, and the tail of every step's log
- the commands that reproduce the job on a local machine
- the runtime E2E commands, which hosted runners cannot run (no nested
  virtualization)

The same text goes to the job summary. Fork pull requests get only the
summary: their token cannot write comments.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import datetime

MARKER = "<!-- firecrab-micromanager-report -->"
BOT_LOGIN = "github-actions[bot]"
# GitHub rejects comment bodies over 65536 characters.
COMMENT_LIMIT = 60000
# Log lines kept per step: passing and failing tail, tried in this order until
# the comment fits.
TAIL_BUDGETS = ((15, 80), (5, 80), (0, 40), (0, 0))
LINE_LIMIT = 300
MICROMANAGER = "firecrab-cli/src/micromanager/"


@dataclass(frozen=True)
class Platform:
    """One hosted microManager job and the files and commands that go with it."""

    name: str
    job: str
    fence: str
    paths: tuple[str, ...]
    local: tuple[str, ...]
    e2e_note: str
    e2e: tuple[str, ...]


MACOS = Platform(
    name="macOS",
    job="microManager (GitHub-hosted macOS build)",
    fence="sh",
    paths=(
        "firecrab-cli/src/micromanager/macos.rs",
        "firecrab-cli/src/micromanager/macos/",
        "micromanager-macos/",
        "scripts/build-micromanager-macos.sh",
        "scripts/ci-qa-macos-e2e.sh",
        "public-docs/micromanager-macos.md",
    ),
    local=(
        "swift test --package-path micromanager-macos"
        " --scratch-path target/swift-micromanager-tests",
        "scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos",
        "cargo build -p firecrab-cli --locked",
        "cargo clippy -p firecrab-cli --all-targets -- -D warnings",
        "cargo test -p firecrab-cli --locked",
        "target/debug/firecrab-micromanager-macos doctor --json",
    ),
    e2e_note=(
        "GitHub-hosted macOS runners have no nested virtualization. "
        "Run the E2E on an Apple silicon M3 or newer Mac:"
    ),
    e2e=(
        'export FIRECRAB_INSTALL_DIR="$PWD/target/e2e/bin"',
        'export FIRECRAB_MICROMANAGER_HOME="$PWD/target/e2e/micromanager"',
        "scripts/ci-qa-macos-e2e.sh all",
        "target/debug/firecrab service uninstall --purge",
    ),
)

WINDOWS = Platform(
    name="Windows",
    job="microManager (GitHub-hosted Windows build)",
    fence="powershell",
    paths=(
        "firecrab-cli/src/micromanager/windows.rs",
        "firecrab-cli/src/micromanager/windows/",
        "scripts/ci-qa-windows-e2e.ps1",
        "scripts/micromanager/create-windows-lab.py",
        "public-docs/micromanager-windows.md",
    ),
    local=(
        "cargo clippy -p firecrab-cli --all-targets -- -D warnings",
        "cargo test -p firecrab-cli --locked",
        "cargo build -p firecrab-cli --locked",
        r".\target\debug\firecrab.exe service doctor --json",
    ),
    e2e_note=(
        "GitHub-hosted Windows runners do not expose nested virtualization to WSL2. "
        "Run the E2E on a Windows host:"
    ),
    e2e=(
        r"scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli target\debug\firecrab.exe",
        r".\target\debug\firecrab.exe service uninstall --purge",
    ),
)

PLATFORMS = (MACOS, WINDOWS)


def is_shared(path: str) -> bool:
    """`micromanager.rs` and the modules beside the platform directories."""
    if path == "firecrab-cli/src/micromanager.rs":
        return True
    if not path.startswith(MICROMANAGER):
        return False
    rest = path[len(MICROMANAGER) :]
    return "/" not in rest and rest not in ("macos.rs", "windows.rs")


def changed_files(platform: Platform, files: list[str]) -> list[str]:
    """The files in `files` that this platform's job builds or documents."""
    return [
        path
        for path in files
        if is_shared(path) or any(path.startswith(prefix) for prefix in platform.paths)
    ]


# --- job logs --------------------------------------------------------------

TIMESTAMP = re.compile(r"^\ufeff?\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z ?")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
ECHOED_COMMAND = "\x1b[36;1m"
USES = re.compile(r"^[\w.-]+/[\w./-]+@\S+$")


@dataclass
class Section:
    """What one step printed: the script it ran and its output."""

    header: str
    commands: list[str] = field(default_factory=list)
    output: list[str] = field(default_factory=list)


def split_log(log: str) -> list[Section]:
    """Cuts a job log into its steps' sections, up to the post-job cleanup."""
    sections: list[Section] = []
    current: Section | None = None
    in_header = False
    for raw in log.splitlines():
        line = TIMESTAMP.sub("", raw)
        if line.startswith("Post job cleanup."):
            break
        if line.startswith("##[group]Run "):
            current = Section(line[len("##[group]Run ") :])
            sections.append(current)
            in_header = True
            continue
        if current is None:
            continue
        if in_header:
            if line.startswith("##[endgroup]"):
                in_header = False
            elif line.startswith(ECHOED_COMMAND):
                current.commands.append(ANSI.sub("", line).rstrip())
            continue
        current.output.append(ANSI.sub("", line).rstrip())
    return sections


def bookkeeping(step: Mapping) -> bool:
    """Runner setup and cleanup steps, which the report leaves out."""
    name = step.get("name", "")
    return name in ("Set up job", "Complete job") or name.startswith("Post ")


@dataclass
class StepLog:
    """One step as the report shows it."""

    name: str
    conclusion: str | None
    seconds: int | None
    section: Section | None


def step_logs(steps: list[Mapping], log: str | None) -> list[StepLog]:
    """Pairs the job's steps with their log sections.

    Every step that ran prints one `Run` section, in order. If the counts
    disagree the pairing is unknown, and the steps are shown without logs.
    """
    shown = [step for step in steps if not bookkeeping(step)]
    executed = [step["number"] for step in shown if step.get("conclusion") != "skipped"]
    sections = split_log(log) if log else []
    by_number = dict(zip(executed, sections, strict=True)) if len(executed) == len(sections) else {}
    result = []
    for step in shown:
        section = by_number.get(step["number"])
        if section is not None and USES.match(section.header):
            # An action's own output (checkout, cache) is noise here.
            section = None
        result.append(StepLog(step["name"], step.get("conclusion"), duration(step), section))
    return result


def duration(item: Mapping) -> int | None:
    """Whole seconds between `started_at` and `completed_at`, if both are known."""
    start, end = item.get("started_at"), item.get("completed_at")
    if not start or not end:
        return None
    return int((parse_time(end) - parse_time(start)).total_seconds())


def parse_time(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


# --- rendering -------------------------------------------------------------

ICONS = {
    "success": "✅",
    "failure": "❌",
    "cancelled": "⏹️",
    "skipped": "⏭️",
    "timed_out": "⌛",
}


def icon(conclusion: str | None) -> str:
    return ICONS.get(conclusion or "", "⏳")


def human_time(seconds: int | None) -> str:
    if seconds is None:
        return "–"
    minutes, seconds = divmod(seconds, 60)
    return f"{minutes}m {seconds}s" if minutes else f"{seconds}s"


def code_block(lines: list[str], language: str) -> str:
    """A fenced block whose fence no line inside can close."""
    longest = max((len(run) for line in lines for run in re.findall(r"`+", line)), default=0)
    fence = "`" * max(3, longest + 1)
    return f"{fence}{language}\n" + "\n".join(lines) + f"\n{fence}"


def tail(section: Section, keep: int) -> list[str]:
    output = section.output
    while output and not output[-1].strip():
        output = output[:-1]
    lines, continued = [], False
    for command in section.commands:
        lines.append(("  " if continued else "$ ") + command)
        # A trailing `\` (sh) or backtick (PowerShell) continues the command.
        continued = command.endswith(("\\", "`"))
    lines += output[-keep:]
    return [line if len(line) <= LINE_LIMIT else line[:LINE_LIMIT] + " …" for line in lines]


@dataclass
class JobReport:
    """One platform's job, the files that brought it into the report, and its steps."""

    platform: Platform
    files: list[str]
    job: Mapping | None
    steps: list[StepLog]


def render(reports: list[JobReport], sha: str, run_url: str, budget: tuple[int, int]) -> str:
    """The comment body; `budget` is the passing and failing log tail length."""
    out = [
        MARKER,
        "## microManager CI",
        "",
        f"Commit `{sha[:7]}` · [workflow run]({run_url}) · updated on every push",
        "",
        "| Platform | Job | Result | Time |",
        "| --- | --- | --- | --- |",
    ]
    for report in reports:
        job = report.job or {}
        name = (
            f"[{report.platform.job}]({job['html_url']})"
            if job.get("html_url")
            else report.platform.job
        )
        conclusion = job.get("conclusion")
        out.append(
            f"| {report.platform.name} | {name} | {icon(conclusion)} {conclusion or 'not run'} "
            f"| {human_time(duration(job))} |"
        )
    for report in reports:
        out += render_platform(report, budget)
    return "\n".join(out) + "\n"


def render_platform(report: JobReport, budget: tuple[int, int]) -> list[str]:
    platform = report.platform
    out = [
        "",
        f"### {platform.name}",
        "",
        f"<details><summary>{len(report.files)} changed {platform.name} file(s)</summary>",
        "",
        *[f"- `{path}`" for path in report.files],
        "",
        "</details>",
        "",
        f"**Reproduce the CI job** on {platform.name}:",
        "",
        code_block(list(platform.local), platform.fence),
        "",
        f"**Runtime E2E.** {platform.e2e_note}",
        "",
        code_block(list(platform.e2e), platform.fence),
    ]
    if not report.steps:
        return out + ["", "The job did not run, so there are no steps or logs."]
    out += ["", "| Step | Result | Time |", "| --- | --- | --- |"]
    out += [
        f"| {step.name} | {icon(step.conclusion)} | {human_time(step.seconds)} |"
        for step in report.steps
    ]
    passing, failing = budget
    for step in report.steps:
        if step.section is None:
            continue
        failed = step.conclusion == "failure"
        keep = failing if failed else passing
        if keep == 0 and not failed:
            continue
        open_ = " open" if failed else ""
        out += [
            "",
            f"<details{open_}><summary>{icon(step.conclusion)} {step.name}: "
            f"log, last {keep} lines</summary>",
            "",
            code_block(tail(step.section, keep), "text"),
            "",
            "</details>",
        ]
    return out


def fit(reports: list[JobReport], sha: str, run_url: str) -> str:
    """The most log that still fits in one comment."""
    for budget in TAIL_BUDGETS:
        body = render(reports, sha, run_url, budget)
        if len(body) <= COMMENT_LIMIT:
            break
    return body


# --- GitHub ----------------------------------------------------------------


class GitHub:
    """The few REST calls the report needs, with the workflow's token."""

    def __init__(self, api: str, repository: str, token: str) -> None:
        self.base = f"{api.rstrip('/')}/repos/{repository}"
        self.token = token

    def request(self, method: str, path: str, body: Mapping | None = None) -> bytes:
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method)
        request.add_header("Accept", "application/vnd.github+json")
        request.add_header("X-GitHub-Api-Version", "2022-11-28")
        # Log downloads redirect to blob storage, which must not get the token.
        request.add_unredirected_header("Authorization", f"Bearer {self.token}")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read()

    def get(self, path: str):
        return json.loads(self.request("GET", path) or b"null")

    def pages(self, path: str) -> list:
        items, page = [], 1
        while True:
            separator = "&" if "?" in path else "?"
            batch = self.get(f"{path}{separator}per_page=100&page={page}")
            items += batch
            if len(batch) < 100:
                return items
            page += 1

    def job_log(self, job_id: int) -> str | None:
        """A finished job's log; it can take a few seconds to be published."""
        for attempt in range(3):
            try:
                return self.request("GET", f"/actions/jobs/{job_id}/logs").decode(
                    "utf-8", "replace"
                )
            except urllib.error.HTTPError as error:
                error.close()
                if error.code != 404 or attempt == 2:
                    print(f"::warning::log of job {job_id}: HTTP {error.code}")
                    return None
                time.sleep(5)
        return None


def own_comment(github: GitHub, number: int) -> Mapping | None:
    for comment in github.pages(f"/issues/{number}/comments"):
        if comment["user"]["login"] == BOT_LOGIN and MARKER in comment["body"]:
            return comment
    return None


def publish(github: GitHub, number: int, body: str | None) -> None:
    """Creates, updates, or (with no body) deletes this report's comment."""
    try:
        existing = own_comment(github, number)
        if body is None:
            if existing:
                github.request("DELETE", f"/issues/comments/{existing['id']}")
        elif existing:
            github.request("PATCH", f"/issues/comments/{existing['id']}", {"body": body})
        else:
            github.request("POST", f"/issues/{number}/comments", {"body": body})
    except urllib.error.HTTPError as error:
        error.close()
        # A fork pull request's token is read-only; the summary still has it.
        print(f"::warning::cannot write the pull request comment: HTTP {error.code}")


def run(env: Mapping[str, str], github: GitHub) -> int:
    """Builds the report for this workflow run's pull request and publishes it."""
    with open(env["GITHUB_EVENT_PATH"], encoding="utf-8") as event_file:
        pull = json.load(event_file).get("pull_request")
    if not pull:
        print("not a pull request; nothing to report")
        return 0

    number = pull["number"]
    files = [item["filename"] for item in github.pages(f"/pulls/{number}/files")]
    touched = [(platform, changed_files(platform, files)) for platform in PLATFORMS]
    touched = [(platform, paths) for platform, paths in touched if paths]
    if not touched:
        print("no microManager platform files changed")
        publish(github, number, None)
        return 0

    run_path = f"/actions/runs/{env['GITHUB_RUN_ID']}/attempts/{env.get('GITHUB_RUN_ATTEMPT', '1')}"
    jobs = github.get(f"{run_path}/jobs?per_page=100")["jobs"]
    reports = []
    for platform, paths in touched:
        job = next((job for job in jobs if job["name"] == platform.job), None)
        log = None
        if job and job.get("conclusion") not in (None, "skipped"):
            log = github.job_log(job["id"])
        steps = step_logs(job.get("steps", []), log) if job else []
        reports.append(JobReport(platform, paths, job, steps))

    server = env.get("GITHUB_SERVER_URL", "https://github.com")
    run_url = f"{server}/{env['GITHUB_REPOSITORY']}/actions/runs/{env['GITHUB_RUN_ID']}"
    body = fit(reports, pull["head"]["sha"], run_url)
    if env.get("GITHUB_STEP_SUMMARY"):
        with open(env["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write(body.replace(MARKER + "\n", "", 1))
    publish(github, number, body)
    print(f"reported {', '.join(platform.name for platform, _ in touched)} on #{number}")
    return 0


def main() -> int:
    env = os.environ
    github = GitHub(
        env.get("GITHUB_API_URL", "https://api.github.com"),
        env["GITHUB_REPOSITORY"],
        env["GITHUB_TOKEN"],
    )
    return run(env, github)


if __name__ == "__main__":
    sys.exit(main())
