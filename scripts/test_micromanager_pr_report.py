#!/usr/bin/env python3
"""Unit tests for scripts/micromanager-pr-report.py (stdlib unittest)."""

from __future__ import annotations

import importlib.util
import io
import json
import re
import sys
import tempfile
import threading
import unittest
import urllib.error
from contextlib import redirect_stdout
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent


def _load_report():
    path = Path(__file__).with_name("micromanager-pr-report.py")
    spec = importlib.util.spec_from_file_location("micromanager_pr_report", path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    # dataclasses look their module up while the class is being built.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


report = _load_report()

CYAN = "\x1b[36;1m"
RESET = "\x1b[0m"


def stamp(lines: list[str]) -> str:
    """A job log as GitHub serves it: BOM, timestamps, CRLF."""
    body = "\r\n".join(f"2026-09-23T08:01:35.2132921Z {line}" for line in lines)
    return "﻿" + body + "\r\n"


LOG = stamp(
    [
        "Current runner version: '2.337.0'",
        "##[group]Runner Image",
        "Image: windows-2025",
        "##[endgroup]",
        "##[group]Run actions/checkout@v7",
        "with:",
        "  repository: SteelCrab/firecrab",
        "##[endgroup]",
        "##[group]Getting Git version info",
        "git version 2.55.0",
        "##[endgroup]",
        "##[group]Run cargo test -p firecrab-cli --locked",
        f"{CYAN}cargo test -p firecrab-cli --locked \\{RESET}",
        f"{CYAN}  --no-fail-fast{RESET}",
        "shell: /usr/bin/bash -e {0}",
        "##[endgroup]",
        "test a ... ok",
        "\x1b[32mtest result: ok. 1 passed\x1b[0m",
        "",
        "##[group]Run ./doctor",
        f"{CYAN}./doctor{RESET}",
        "##[endgroup]",
        "check failed",
        "##[error]Process completed with exit code 1.",
        "Post job cleanup.",
        "##[group]Run actions/checkout@v7 post",
        "should not be read",
    ]
)

STEPS = [
    {"number": 1, "name": "Set up job", "conclusion": "success"},
    {"number": 2, "name": "Run actions/checkout@v7", "conclusion": "success"},
    {
        "number": 3,
        "name": "Tests",
        "conclusion": "success",
        "started_at": "2026-09-23T08:01:35Z",
        "completed_at": "2026-09-23T08:03:20Z",
    },
    {"number": 4, "name": "Doctor", "conclusion": "failure"},
    {"number": 5, "name": "Never reached", "conclusion": "skipped"},
    {"number": 6, "name": "Post Run actions/checkout@v7", "conclusion": "success"},
    {"number": 7, "name": "Complete job", "conclusion": "success"},
]


class ChangedFilesTests(unittest.TestCase):
    def test_platform_files_belong_to_their_platform_only(self) -> None:
        files = [
            "firecrab-cli/src/micromanager/windows/wsl.rs",
            "micromanager-macos/Sources/FirecrabMicroManager/Launcher.swift",
            "scripts/ci-qa-windows-e2e.ps1",
        ]
        self.assertEqual(
            report.changed_files(report.WINDOWS, files),
            ["firecrab-cli/src/micromanager/windows/wsl.rs", "scripts/ci-qa-windows-e2e.ps1"],
        )
        self.assertEqual(
            report.changed_files(report.MACOS, files),
            ["micromanager-macos/Sources/FirecrabMicroManager/Launcher.swift"],
        )

    def test_shared_micromanager_modules_belong_to_both(self) -> None:
        for shared in (
            "firecrab-cli/src/micromanager.rs",
            "firecrab-cli/src/micromanager/artifact.rs",
        ):
            for platform in report.PLATFORMS:
                self.assertEqual(report.changed_files(platform, [shared]), [shared])

    def test_the_platform_roots_are_not_shared(self) -> None:
        self.assertEqual(
            report.changed_files(report.MACOS, ["firecrab-cli/src/micromanager/windows.rs"]),
            [],
        )

    def test_other_files_bring_in_no_platform(self) -> None:
        files = ["firecrab-api/src/main.rs", "install-cli.ps1", "README.md"]
        for platform in report.PLATFORMS:
            self.assertEqual(report.changed_files(platform, files), [])


class LogTests(unittest.TestCase):
    def test_a_log_splits_into_run_sections_until_the_cleanup(self) -> None:
        sections = report.split_log(LOG)
        self.assertEqual(
            [section.header for section in sections],
            ["actions/checkout@v7", "cargo test -p firecrab-cli --locked", "./doctor"],
        )
        tests = sections[1]
        self.assertEqual(
            tests.commands, ["cargo test -p firecrab-cli --locked \\", "  --no-fail-fast"]
        )
        self.assertEqual(tests.output, ["test a ... ok", "test result: ok. 1 passed", ""])
        self.assertEqual(
            sections[2].output,
            ["check failed", "##[error]Process completed with exit code 1."],
        )

    def test_steps_pair_with_sections_and_actions_show_no_log(self) -> None:
        steps = report.step_logs(STEPS, LOG)
        self.assertEqual(
            [step.name for step in steps],
            ["Run actions/checkout@v7", "Tests", "Doctor", "Never reached"],
        )
        checkout, tests, doctor, skipped = steps
        self.assertIsNone(checkout.section)
        self.assertEqual(tests.section.header, "cargo test -p firecrab-cli --locked")
        self.assertEqual(tests.seconds, 105)
        self.assertEqual(doctor.section.header, "./doctor")
        self.assertIsNone(skipped.section)

    def test_an_unexpected_section_count_shows_steps_without_logs(self) -> None:
        steps = report.step_logs(STEPS[:3], LOG)
        self.assertTrue(all(step.section is None for step in steps))
        self.assertTrue(all(step.section is None for step in report.step_logs(STEPS, None)))


class RenderTests(unittest.TestCase):
    def job_report(self, platform=None, log: str = LOG) -> object:
        job = {
            "html_url": "https://github.com/o/r/actions/runs/1/job/2",
            "conclusion": "failure",
            "started_at": "2026-09-23T08:00:00Z",
            "completed_at": "2026-09-23T08:02:24Z",
        }
        return report.JobReport(
            platform or report.WINDOWS,
            ["firecrab-cli/src/micromanager/windows/wsl.rs"],
            job,
            report.step_logs(STEPS, log),
        )

    def test_the_comment_has_commands_steps_and_the_failed_log(self) -> None:
        body = report.render([self.job_report()], "0123456789abcdef", "https://run", (15, 80))
        self.assertTrue(body.startswith(report.MARKER))
        self.assertIn("Commit `0123456` · [workflow run](https://run)", body)
        self.assertIn("| Windows | [microManager (GitHub-hosted Windows build)]", body)
        self.assertIn("❌ failure | 2m 24s |", body)
        for command in report.WINDOWS.local + report.WINDOWS.e2e:
            self.assertIn(command, body)
        self.assertIn("| Tests | ✅ | 1m 45s |", body)
        self.assertIn("| Never reached | ⏭️ | – |", body)
        self.assertIn("<details open><summary>❌ Doctor: log, last 80 lines", body)
        self.assertIn("$ cargo test -p firecrab-cli --locked \\\n    --no-fail-fast", body)
        self.assertIn("##[error]Process completed with exit code 1.", body)
        self.assertNotIn("should not be read", body)

    def fit_under(self, budget: tuple[int, int]) -> str:
        """`fit` with the limit set to exactly what `budget` renders to."""
        limit = len(report.render([self.job_report()], "0" * 40, "https://run", budget))
        original = report.COMMENT_LIMIT
        report.COMMENT_LIMIT = limit
        try:
            return report.fit([self.job_report()], "0" * 40, "https://run")
        finally:
            report.COMMENT_LIMIT = original

    def test_passing_logs_shrink_first_when_the_comment_is_too_long(self) -> None:
        body = self.fit_under((5, 80))
        self.assertIn("✅ Tests: log, last 5 lines", body)
        self.assertIn("❌ Doctor: log, last 80 lines", body)

        body = self.fit_under((0, 40))
        self.assertNotIn("✅ Tests: log", body)
        self.assertIn("❌ Doctor: log, last 40 lines", body)

    def test_a_job_that_never_ran_says_so(self) -> None:
        missing = report.JobReport(report.MACOS, ["micromanager-macos/Package.swift"], None, [])
        body = report.render([missing], "0" * 40, "https://run", (15, 80))
        self.assertIn("| macOS | microManager (GitHub-hosted macOS build) | ⏳ not run | – |", body)
        self.assertIn("The job did not run", body)

    def test_log_backticks_cannot_close_the_code_block(self) -> None:
        block = report.code_block(["```", "a ```` b"], "text")
        self.assertTrue(block.startswith("`````text\n"))
        self.assertTrue(block.endswith("\n`````"))

    def test_long_log_lines_are_clipped(self) -> None:
        section = report.Section("x", [], ["y" * (report.LINE_LIMIT + 50)])
        (line,) = report.tail(section, 5)
        self.assertEqual(len(line), report.LINE_LIMIT + 2)
        self.assertTrue(line.endswith(" …"))


class CiContractTests(unittest.TestCase):
    """The report names ci.yml's jobs and repeats their commands; keep them in step."""

    ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

    def job_block(self, name: str) -> str:
        lines = self.ci.splitlines()
        start = lines.index(f"    name: {name}")
        end = next(
            (i for i in range(start + 1, len(lines)) if re.match(r"^  \S", lines[i])),
            len(lines),
        )
        return " ".join(" ".join(lines[start:end]).split())

    def test_local_commands_are_what_the_job_runs(self) -> None:
        for platform in report.PLATFORMS:
            block = self.job_block(platform.job)
            for command in platform.local:
                normalized = command.removeprefix(".\\").replace("\\", "/")
                self.assertIn(normalized, block, f"{platform.name}: {command}")

    def test_the_report_job_waits_for_both_platform_jobs(self) -> None:
        block = self.job_block("microManager PR report")
        self.assertIn("needs: [micromanager-macos-build, micromanager-windows-build]", block)
        self.assertIn("python3 scripts/micromanager-pr-report.py", block)
        self.assertIn("pull-requests: write", block)

    def test_every_platform_path_exists(self) -> None:
        for platform in report.PLATFORMS:
            for path in platform.paths:
                self.assertTrue((ROOT / path).exists(), path)
            for command in platform.e2e:
                for script in re.findall(r"scripts[\\/][\w./\\-]+", command):
                    self.assertTrue((ROOT / script.replace("\\", "/")).exists(), script)


class ApiHandler(BaseHTTPRequestHandler):
    """A stand-in for api.github.com and the blob store logs redirect to."""

    seen: list[tuple[str, str, str | None, bytes]] = []

    def log_message(self, *args) -> None:
        pass

    def reply(self, code: int, body: bytes, headers: dict | None = None) -> None:
        self.send_response(code)
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def record(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        self.seen.append((self.command, self.path, self.headers.get("Authorization"), body))

    def do_GET(self) -> None:
        self.record()
        if self.path.startswith("/repos/o/r/pulls/5/files"):
            page = int(self.path.rsplit("page=", 1)[1])
            count = 100 if page == 1 else 1
            files = [{"filename": f"f{page}-{i}"} for i in range(count)]
            return self.reply(200, json.dumps(files).encode())
        if self.path == "/repos/o/r/actions/jobs/9/logs":
            return self.reply(302, b"", {"Location": "/blob/9.txt"})
        if self.path == "/blob/9.txt":
            return self.reply(200, "log ✓".encode())
        self.reply(404, b"{}")

    def do_POST(self) -> None:
        self.record()
        self.reply(201, b"{}")


class GitHubClientTests(unittest.TestCase):
    def setUp(self) -> None:
        ApiHandler.seen = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), ApiHandler)
        threading.Thread(
            target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
        ).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        api = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.github = report.GitHub(api, "o/r", "secret")

    def test_lists_follow_pages_until_a_short_one(self) -> None:
        files = self.github.pages("/pulls/5/files")
        self.assertEqual(len(files), 101)
        self.assertEqual(
            [path for _, path, _, _ in ApiHandler.seen],
            [
                "/repos/o/r/pulls/5/files?per_page=100&page=1",
                "/repos/o/r/pulls/5/files?per_page=100&page=2",
            ],
        )

    def test_a_log_redirect_does_not_carry_the_token(self) -> None:
        self.assertEqual(self.github.job_log(9), "log ✓")
        (api, blob) = ApiHandler.seen
        self.assertEqual(api[2], "Bearer secret")
        self.assertEqual((blob[1], blob[2]), ("/blob/9.txt", None))

    def test_a_missing_log_is_retried_then_reported(self) -> None:
        output = io.StringIO()
        with mock.patch.object(report.time, "sleep") as sleep, redirect_stdout(output):
            self.assertIsNone(self.github.job_log(8))
        self.assertEqual(len(ApiHandler.seen), 3)
        self.assertEqual(sleep.call_count, 2)
        self.assertIn("::warning::log of job 8: HTTP 404", output.getvalue())

    def test_writes_send_json_with_the_token(self) -> None:
        self.github.request("POST", "/issues/5/comments", {"body": "hi"})
        ((method, path, token, body),) = ApiHandler.seen
        self.assertEqual(
            (method, path, token), ("POST", "/repos/o/r/issues/5/comments", "Bearer secret")
        )
        self.assertEqual(json.loads(body), {"body": "hi"})


class FakeGitHub:
    """Records writes; serves canned files, jobs, logs, and comments."""

    def __init__(self, files=(), jobs=(), logs=None, comments=(), refuse_writes=False):
        self.files = [{"filename": name} for name in files]
        self.jobs = list(jobs)
        self.logs = logs or {}
        self.comments = list(comments)
        self.refuse_writes = refuse_writes
        self.writes: list[tuple[str, str, object]] = []

    def pages(self, path: str) -> list:
        if path.startswith("/pulls/"):
            return self.files
        return self.comments

    def get(self, path: str):
        assert path == "/actions/runs/77/attempts/2/jobs?per_page=100", path
        return {"jobs": self.jobs}

    def job_log(self, job_id: int) -> str | None:
        return self.logs.get(job_id)

    def request(self, method: str, path: str, body=None) -> bytes:
        if self.refuse_writes:
            raise urllib.error.HTTPError(path, 403, "Forbidden", None, None)
        self.writes.append((method, path, body))
        return b"{}"


def comment(identifier: int, login: str, body: str) -> dict:
    return {"id": identifier, "user": {"login": login}, "body": body}


class PublishTests(unittest.TestCase):
    def test_a_first_report_creates_the_comment(self) -> None:
        github = FakeGitHub(comments=[comment(1, "someone", "lgtm")])
        report.publish(github, 5, "body")
        self.assertEqual(github.writes, [("POST", "/issues/5/comments", {"body": "body"})])

    def test_a_later_push_edits_the_bots_own_comment_only(self) -> None:
        quoted = comment(1, "someone", f"quoting {report.MARKER}")
        own = comment(2, report.BOT_LOGIN, f"{report.MARKER}\nold")
        github = FakeGitHub(comments=[quoted, own])
        report.publish(github, 5, "new")
        self.assertEqual(github.writes, [("PATCH", "/issues/comments/2", {"body": "new"})])

    def test_no_report_removes_a_stale_comment(self) -> None:
        github = FakeGitHub(comments=[comment(3, report.BOT_LOGIN, report.MARKER)])
        report.publish(github, 5, None)
        self.assertEqual(github.writes, [("DELETE", "/issues/comments/3", None)])
        github = FakeGitHub()
        report.publish(github, 5, None)
        self.assertEqual(github.writes, [])

    def test_a_read_only_token_warns_instead_of_failing(self) -> None:
        output = io.StringIO()
        with redirect_stdout(output):
            report.publish(FakeGitHub(refuse_writes=True), 5, "body")
        self.assertIn(
            "::warning::cannot write the pull request comment: HTTP 403", output.getvalue()
        )


class RunTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.summary = self.root / "summary.md"
        self.summary.write_text("", encoding="utf-8")

    def env(self, event: dict) -> dict:
        path = self.root / "event.json"
        path.write_text(json.dumps(event), encoding="utf-8")
        return {
            "GITHUB_EVENT_PATH": str(path),
            "GITHUB_REPOSITORY": "SteelCrab/firecrab",
            "GITHUB_RUN_ID": "77",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_SERVER_URL": "https://github.com",
            "GITHUB_STEP_SUMMARY": str(self.summary),
        }

    def run_report(self, github: FakeGitHub, event: dict) -> str:
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(report.run(self.env(event), github), 0)
        return output.getvalue()

    def test_a_windows_change_reports_the_windows_job(self) -> None:
        job = {
            "id": 9,
            "name": report.WINDOWS.job,
            "conclusion": "failure",
            "html_url": "https://github.com/o/r/actions/runs/77/job/9",
            "steps": STEPS,
        }
        github = FakeGitHub(
            files=["firecrab-cli/src/micromanager/windows/doctor.rs"],
            jobs=[job, {"id": 8, "name": report.MACOS.job, "conclusion": "success"}],
            logs={9: LOG},
        )
        event = {"pull_request": {"number": 5, "head": {"sha": "abcdef0123"}}}
        self.assertIn("reported Windows on #5", self.run_report(github, event))

        ((method, path, body),) = github.writes
        self.assertEqual((method, path), ("POST", "/issues/5/comments"))
        self.assertIn("### Windows", body["body"])
        self.assertNotIn("### macOS", body["body"])
        self.assertIn("https://github.com/SteelCrab/firecrab/actions/runs/77", body["body"])
        summary = self.summary.read_text(encoding="utf-8")
        self.assertTrue(summary.startswith("## microManager CI"))
        self.assertNotIn(report.MARKER, summary)

    def test_an_unrelated_change_writes_nothing(self) -> None:
        github = FakeGitHub(files=["firecrab-api/src/main.rs"])
        event = {"pull_request": {"number": 5, "head": {"sha": "abc"}}}
        self.assertIn("no microManager platform files changed", self.run_report(github, event))
        self.assertEqual(github.writes, [])
        self.assertEqual(self.summary.read_text(encoding="utf-8"), "")

    def test_other_events_are_ignored(self) -> None:
        self.assertIn("not a pull request", self.run_report(FakeGitHub(), {"ref": "main"}))


if __name__ == "__main__":
    unittest.main()
