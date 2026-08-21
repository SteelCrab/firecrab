#!/usr/bin/env python3
"""Unit tests for scripts/write-release-notes.py."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from textwrap import dedent


def _load():
    path = Path(__file__).with_name("write-release-notes.py")
    spec = importlib.util.spec_from_file_location("write_release_notes", path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


notes = _load()


CHANGELOG = dedent(
    """\
    # Changelog

    ## [0.1.1] - 2026-08-17

    Second release.

    ### Added

    - Host install from a GitHub Release ([#12]).
    - [`public-docs/oci.md`](public-docs/oci.md) documents OCI import.

    ### Changed

    - None.

    ### Deprecated

    - None.

    ### Fixed

    - None.

    ### Improved

    - None.

    ## [0.1.0] - 2026-08-16

    First public release.

    ### Added

    - Host install from a GitHub Release.

    ### Changed

    - None.

    ### Deprecated

    - None.

    ### Fixed

    - None.

    ### Improved

    - None.

    [#12]: https://github.com/SteelCrab/firecrab/issues/12
    """
)


def _fake_runner(calls: list[list[str]], stdout: str):
    """Record the git argv and replay canned `git shortlog` output."""

    def run(argv: list[str]) -> str:
        calls.append(argv)
        return stdout

    return run


class WriteReleaseNotesTests(unittest.TestCase):
    def test_body_has_install_url_contributors_and_changelog(self) -> None:
        body = notes.build_notes(
            tag="v0.1.1",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab", "kudala-bharani"],
            repo="SteelCrab/firecrab",
        )
        self.assertIn(
            "curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash",
            body,
        )
        self.assertIn(
            "https://github.com/SteelCrab/firecrab/releases/download/v0.1.1/install.sh",
            body,
        )
        self.assertNotIn("## Binaries", body)
        self.assertNotIn("API + helper only", body)
        for asset in (
            "firecrab-host-x86_64-gnu.tar.gz",
            "firecrab-x86_64-unknown-linux-gnu.tar.gz",
            "firecrab-x86_64-unknown-linux-musl.tar.gz",
            "firecrab-aarch64-unknown-linux-gnu.tar.gz",
            "firecrab-aarch64-unknown-linux-musl.tar.gz",
        ):
            self.assertNotIn(asset, body)
        self.assertIn("./install.sh --libc gnu", body)
        self.assertIn("./install.sh --libc musl", body)
        self.assertIn("SteelCrab", body)
        self.assertIn("kudala-bharani", body)
        self.assertIn("## Changelog", body)
        self.assertIn("### Added", body)
        self.assertIn("Second release.", body)
        self.assertIn('<p align="left">', body)
        self.assertNotIn('align="center"', body)
        self.assertIn('src="https://github.com/SteelCrab.png?size=96"', body)
        self.assertIn('src="https://github.com/kudala-bharani.png?size=96"', body)
        self.assertIn('href="https://github.com/SteelCrab"', body)
        self.assertNotIn("- SteelCrab", body)
        self.assertGreater(body.rfind("## Contributors"), body.rfind("## Changelog"))
        self.assertTrue(body.rstrip().endswith("</p>"))

    def test_release_name_carries_the_project_and_tag(self) -> None:
        self.assertEqual(notes.release_name("v0.1.2"), "firecrab v0.1.2")
        self.assertEqual(notes.release_name("0.1.2"), "firecrab v0.1.2")

    def test_body_does_not_repeat_the_release_title(self) -> None:
        # The release page prints release_name() above the body, so an H1
        # saying the same thing renders as a duplicate title.
        body = notes.build_notes(
            tag="v0.1.1",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab"],
            repo="SteelCrab/firecrab",
        )
        self.assertTrue(body.startswith("## Install"), body[:40])
        self.assertNotIn("# firecrab v0.1.1", body)
        self.assertNotIn("# firecrab 0.1.1", body)

    def test_changelog_section_holds_no_second_version_heading(self) -> None:
        body = notes.build_notes(
            tag="v0.1.1",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab"],
            repo="SteelCrab/firecrab",
        )
        self.assertNotIn("## [0.1.1] - 2026-08-17", body)
        self.assertIn("Second release.", body)
        self.assertIn("### Added", body)

    def test_issue_references_get_their_link_definitions(self) -> None:
        # Without the definition GitHub renders a bare "[#12]".
        body = notes.build_notes(
            tag="v0.1.1",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab"],
            repo="SteelCrab/firecrab",
        )
        self.assertIn("[#12]: https://github.com/SteelCrab/firecrab/issues/12", body)

    def test_repository_paths_become_absolute_links(self) -> None:
        # A release page is not a tree page, so "public-docs/oci.md" 404s.
        body = notes.build_notes(
            tag="v0.1.1",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab"],
            repo="SteelCrab/firecrab",
        )
        self.assertIn(
            "](https://github.com/SteelCrab/firecrab/blob/v0.1.1/public-docs/oci.md)",
            body,
        )
        self.assertNotIn("](public-docs/oci.md)", body)

    def test_skips_bot_contributors(self) -> None:
        names = notes.filter_contributors(["SteelCrab", "dependabot[bot]", "github-actions[bot]"])
        self.assertEqual(names, ["SteelCrab"])

    def test_git_contributors_defaults_to_whole_history(self) -> None:
        calls: list[list[str]] = []
        self.assertEqual(
            notes.git_contributors(Path("/repo"), runner=_fake_runner(calls, "  2\tSteelCrab\n")),
            ["SteelCrab"],
        )
        self.assertIn("--all", calls[0])

    def test_git_contributors_limits_to_a_release_range(self) -> None:
        # A release lists who worked on *that* release, so an earlier
        # release's contributors must not reappear.
        calls: list[list[str]] = []
        names = notes.git_contributors(
            Path("/repo"),
            since="v0.1.1",
            runner=_fake_runner(calls, "  83\tSteelCrab\n   1\tdependabot[bot]\n"),
        )
        self.assertEqual(names, ["SteelCrab"])
        self.assertIn("v0.1.1..HEAD", calls[0])
        self.assertNotIn("--all", calls[0])

    def test_writes_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "notes.md"
            notes.write_notes(
                tag="v0.1.1",
                changelog_text=CHANGELOG,
                contributors=["SteelCrab"],
                repo="SteelCrab/firecrab",
                dest=out,
            )
            text = out.read_text(encoding="utf-8")
            self.assertTrue(text.startswith("## Install\n"))


if __name__ == "__main__":
    unittest.main()
