#!/usr/bin/env python3
"""Behavior tests for README contributor collection and publication."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from textwrap import dedent
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("update-contributors.py")
SPEC = importlib.util.spec_from_file_location("update_contributors", SCRIPT)
assert SPEC and SPEC.loader
credits = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = credits
SPEC.loader.exec_module(credits)

REPOSITORY = "owner/project"
README = "Intro\n\n" + credits.START + "\n\nOld credits\n\n" + credits.END + "\n\nLicense\n"


def contribution(number, login="alice", user_id=1, *, pr=False, labels=(), user_type="User"):
    item = {
        "number": number,
        "user": {"id": user_id, "login": login, "type": user_type},
        "labels": [{"name": name} for name in labels],
        "state": "closed",
    }
    if pr:
        item["pull_request"] = {"merged_at": None}
    return item


class CollectionTests(unittest.TestCase):
    def test_pr_issue_and_issue_only_contributors_receive_source_links(self):
        items = [
            contribution(1, pr=True),
            contribution(2),
            contribution(3, "bob", 2),
        ]
        items[2]["state"] = "open"
        people = credits.collect_contributors(items)
        self.assertEqual(len(people), 2)
        self.assertEqual(people[1].pulls, {1})
        self.assertEqual(people[1].issues, {2})
        self.assertEqual(people[2].pulls, set())
        table = credits.render_contributors(REPOSITORY, people)
        for source in ["pull/1", "issues/2", "issues/3"]:
            self.assertIn(f"https://github.com/{REPOSITORY}/{source}", table)
        self.assertIn("[@bob](https://github.com/bob)", table)

    def test_repeated_events_and_username_changes_keep_one_account(self):
        old = contribution(1, "old-name", pr=True)
        new = contribution(2, "new-name")
        people = credits.collect_contributors([new, old, new, old])
        self.assertEqual(len(people), 1)
        self.assertEqual(people[1].login, "new-name")
        self.assertEqual(people[1].pulls, {1})
        self.assertEqual(people[1].issues, {2})

    def test_bots_deleted_users_spam_and_opt_out_are_excluded(self):
        deleted = contribution(1)
        deleted["user"] = None
        items = [
            deleted,
            contribution(2, "ghost", 10137),
            contribution(3, "dependabot[bot]", 3, user_type="Bot"),
            contribution(4, labels=["SPAM"]),
            contribution(5, labels=["Skip-Contributors"]),
            contribution(6, "robotics-human", 6, labels=["duplicate"]),
        ]
        self.assertEqual(set(credits.collect_contributors(items)), {6})

    def test_exclusion_removes_only_the_labeled_contribution(self):
        items = [contribution(1, pr=True), contribution(2)]
        self.assertTrue(credits.collect_contributors(items)[1].issues)
        items[1]["labels"] = [{"name": "skip-contributors"}]
        person = credits.collect_contributors(items)[1]
        self.assertEqual(person.pulls, {1})
        self.assertEqual(person.issues, set())
        items[1]["labels"] = []
        self.assertEqual(credits.collect_contributors(items)[1].issues, {2})

    def test_output_is_sorted_and_independent_of_page_order(self):
        items = [contribution(1, "Zoe", 2), contribution(2, "alice", 1)]
        table = credits.render_contributors(REPOSITORY, credits.collect_contributors(items))
        reversed_table = credits.render_contributors(REPOSITORY, credits.collect_contributors(items[::-1]))
        self.assertEqual(table, reversed_table)
        self.assertLess(table.index("[@alice]"), table.index("[@Zoe]"))

    def test_large_histories_show_counts_and_link_to_history_and_latest_source(self):
        people = credits.collect_contributors([contribution(number) for number in range(1, 251)])
        table = credits.render_contributors(REPOSITORY, people)
        self.assertIn("[250](https://github.com/owner/project/issues?q=is%3Aissue+author%3Aalice", table)
        self.assertIn("[latest #250](https://github.com/owner/project/issues/250)", table)
        self.assertIn("-label%3Aspam+-label%3Askip-contributors", table)

    def test_untrusted_titles_urls_and_bodies_are_never_rendered(self):
        item = contribution(1)
        item.update({"title": "$(touch /tmp/no)", "body": "<script>bad()</script>", "html_url": "javascript:bad()"})
        table = credits.render_contributors(REPOSITORY, credits.collect_contributors([item]))
        self.assertNotIn("touch", table)
        self.assertNotIn("script", table)
        self.assertNotIn("javascript:", table)
        self.assertIn("https://github.com/owner/project/issues/1", table)

    def test_invalid_author_or_number_is_rejected(self):
        for item in [contribution(1, '<img src="bad">'), contribution(-1), contribution(1, user_id=True)]:
            with self.subTest(item=item), self.assertRaises(ValueError):
                credits.collect_contributors([item])


class FetchTests(unittest.TestCase):
    def test_full_history_is_flattened_from_all_pages(self):
        pages = [[contribution(1, pr=True)], [contribution(2)], []]
        with patch.object(credits.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(pages))) as run:
            items = credits.fetch_contributions(REPOSITORY)
        self.assertEqual([item["number"] for item in items], [1, 2])
        command = run.call_args.args[0]
        self.assertIn("--paginate", command)
        self.assertIn("--slurp", command)
        self.assertIn("state=all", command[-1])
        self.assertTrue(run.call_args.kwargs["check"])

    def test_error_object_cannot_be_treated_as_an_empty_history(self):
        with patch.object(credits.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, '{"message":"rate limited"}')):
            with self.assertRaises(ValueError):
                credits.fetch_contributions(REPOSITORY)


class ReadmeTests(unittest.TestCase):
    def test_only_generated_region_changes_and_second_run_is_identical(self):
        updated = credits.replace_contributors(README, "New credits")
        self.assertEqual(updated, README.replace("Old credits", "New credits"))
        self.assertEqual(credits.replace_contributors(updated, "New credits"), updated)

    def test_missing_duplicate_inline_and_reversed_markers_are_rejected(self):
        for text in [
            "No markers",
            README + credits.START + "\n",
            README.replace(credits.END, ""),
            README.replace(credits.START, "inline " + credits.START),
            "inline " + credits.START + "\n" + README,
            credits.END + "\n" + credits.START + "\n",
        ]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                credits.replace_contributors(text, "new")

    def run_main(self, readme, *args):
        with redirect_stdout(StringIO()), redirect_stderr(StringIO()):
            return credits.main(["--repository", REPOSITORY, "--readme", str(readme), *args])

    def test_cli_backfills_then_skips_unchanged_file_and_check_does_not_write(self):
        with tempfile.TemporaryDirectory() as directory:
            readme = Path(directory) / "README.md"
            readme.write_text(README)
            with patch.object(credits, "fetch_contributions", return_value=[contribution(1)]):
                self.assertEqual(self.run_main(readme, "--check"), 1)
                self.assertEqual(readme.read_text(), README)
                self.assertEqual(self.run_main(readme), 0)
                updated = readme.read_text()
                modified = readme.stat().st_mtime_ns
                self.assertEqual(self.run_main(readme), 0)
                self.assertEqual(self.run_main(readme, "--check"), 0)
                self.assertEqual(readme.read_text(), updated)
                self.assertEqual(readme.stat().st_mtime_ns, modified)

    def test_api_failure_or_malformed_data_preserves_existing_credits(self):
        errors = [
            subprocess.CalledProcessError(1, ["gh"], stderr="API rate limit exceeded"),
            subprocess.TimeoutExpired(["gh"], 120),
            ValueError("invalid API response"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            readme = Path(directory) / "README.md"
            readme.write_text(README)
            for error in errors:
                with self.subTest(error=error), patch.object(credits, "fetch_contributions", side_effect=error):
                    self.assertEqual(self.run_main(readme), 1)
                    self.assertEqual(readme.read_text(), README)
            with patch.object(credits, "fetch_contributions", return_value=[{"number": 1}]):
                self.assertEqual(self.run_main(readme), 1)
                self.assertEqual(readme.read_text(), README)

    def test_bad_markers_fail_before_any_api_request(self):
        with tempfile.TemporaryDirectory() as directory:
            readme = Path(directory) / "README.md"
            readme.write_text("No markers\n")
            with patch.object(credits, "fetch_contributions") as fetch:
                self.assertEqual(self.run_main(readme), 1)
            fetch.assert_not_called()
            self.assertEqual(readme.read_text(), "No markers\n")

    def test_deleted_contributions_disappear_from_the_next_snapshot(self):
        people = credits.collect_contributors([contribution(1, "alice", 1), contribution(2, "bob", 2)])
        initial = credits.replace_contributors(README, credits.render_contributors(REPOSITORY, people))
        people = credits.collect_contributors([contribution(2, "bob", 2)])
        updated = credits.replace_contributors(initial, credits.render_contributors(REPOSITORY, people))
        self.assertNotIn("[@alice]", updated)
        self.assertIn("[@bob]", updated)


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.origin = self.root / "origin.git"
        self.checkout = self.root / "checkout"
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.env.update(
            GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.invalid",
            DEFAULT_BRANCH="main", GITHUB_REPOSITORY=REPOSITORY,
        )
        self.git(self.root, "init", "--bare", "--initial-branch=main", str(self.origin))
        self.git(self.root, "clone", str(self.origin), str(self.checkout))
        (self.checkout / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.checkout / "scripts" / SCRIPT.name)
        (self.checkout / "README.md").write_text(README)
        self.git(self.checkout, "add", ".")
        self.git(self.checkout, "commit", "-m", "Initial fixture")
        self.git(self.checkout, "push", "origin", "main")
        binaries = self.root / "bin"
        binaries.mkdir()
        fixture = self.root / "contributions.json"
        fixture.write_text(json.dumps([[contribution(1, pr=True), contribution(2, "bob", 2)]]))
        fake_gh = binaries / "gh"
        fake_gh.write_text(f"#!{sys.executable}\n" + dedent('''\
            import os
            import subprocess
            import sys
            from pathlib import Path

            race = os.environ.get("COMPETING_CLONE")
            if race and not Path(race, "pushed").exists():
                checkout = Path(race)
                readme = checkout / "README.md"
                readme.write_text("Concurrent introduction\\n" + readme.read_text())
                for arguments in [["add", "README.md"], ["commit", "-m", "Concurrent change"], ["push", "origin", "main"]]:
                    subprocess.run(["git", "-C", str(checkout), *arguments], check=True, capture_output=True)
                (checkout / "pushed").touch()
            print(Path(os.environ["CONTRIBUTION_FIXTURE"]).read_text())
            if os.environ.get("FAIL_API"):
                print("API rate limit exceeded", file=sys.stderr)
                sys.exit(1)
            '''))
        fake_gh.chmod(0o755)
        self.env.update(PATH=str(binaries) + os.pathsep + self.env["PATH"], CONTRIBUTION_FIXTURE=str(fixture))
        workflow = SCRIPT.parent.parent / ".github" / "workflows" / "contributors.yml"
        self.publish = dedent(workflow.read_text().split("        run: |\n", 1)[1])

    def git(self, checkout, *arguments):
        return subprocess.run(
            ["git", "-C", str(checkout), *arguments], env=self.env,
            check=True, capture_output=True, text=True, timeout=30,
        ).stdout.strip()

    def run_workflow(self):
        return subprocess.run(
            ["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", self.publish],
            cwd=self.checkout, env=self.env, capture_output=True, text=True, timeout=30,
        )

    def test_publish_then_rerun_creates_only_one_credit_commit(self):
        for attempt in range(2):
            result = self.run_workflow()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(self.git(self.origin, "rev-list", "--count", "main"), "2")
        readme = self.git(self.origin, "show", "main:README.md")
        self.assertIn("pull/1", readme)
        self.assertIn("[@bob]", readme)
        self.assertIn("issues/2", readme)
        self.assertTrue(readme.startswith("Intro\n"))
        self.assertTrue(readme.endswith("License"))

    def test_concurrent_push_is_retried_without_losing_the_readme_edit(self):
        competing = self.root / "competitor"
        self.git(self.root, "clone", str(self.origin), str(competing))
        self.env["COMPETING_CLONE"] = str(competing)
        result = self.run_workflow()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("push attempt 1 failed", result.stdout)
        self.assertEqual(self.git(self.origin, "rev-list", "--count", "main"), "3")
        readme = self.git(self.origin, "show", "main:README.md")
        self.assertTrue(readme.startswith("Concurrent introduction\nIntro\n"))
        self.assertIn("pull/1", readme)

    def test_partial_api_failure_cannot_publish_partial_or_empty_credits(self):
        self.env["FAIL_API"] = "1"
        result = self.run_workflow()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("API rate limit exceeded", result.stderr)
        self.assertEqual(self.git(self.origin, "rev-list", "--count", "main"), "1")
        self.assertEqual(self.git(self.origin, "show", "main:README.md"), README.strip())


if __name__ == "__main__":
    unittest.main()
