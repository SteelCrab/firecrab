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
    """
)


class WriteReleaseNotesTests(unittest.TestCase):
    def test_body_has_install_url_all_binaries_contributors_and_changelog(self) -> None:
        body = notes.build_notes(
            tag="v0.1.0",
            changelog_text=CHANGELOG,
            contributors=["SteelCrab", "kudala-bharani"],
            repo="SteelCrab/firecrab",
        )
        self.assertIn(
            "curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash",
            body,
        )
        self.assertIn(
            "https://github.com/SteelCrab/firecrab/releases/download/v0.1.0/install.sh",
            body,
        )
        for asset in (
            "firecrab-host-x86_64-gnu.tar.gz",
            "firecrab-host-x86_64-musl.tar.gz",
            "firecrab-host-aarch64-gnu.tar.gz",
            "firecrab-host-aarch64-musl.tar.gz",
            "firecrab-x86_64-unknown-linux-gnu.tar.gz",
            "firecrab-x86_64-unknown-linux-musl.tar.gz",
            "firecrab-aarch64-unknown-linux-gnu.tar.gz",
            "firecrab-aarch64-unknown-linux-musl.tar.gz",
        ):
            self.assertIn(asset, body)
        self.assertIn("./install.sh --libc gnu", body)
        self.assertIn("./install.sh --libc musl", body)
        self.assertIn("SteelCrab", body)
        self.assertIn("kudala-bharani", body)
        self.assertIn("## Changelog", body)
        self.assertIn("### Added", body)
        self.assertIn("First public release.", body)
        self.assertIn('<p align="left">', body)
        self.assertNotIn('align="center"', body)
        self.assertIn('src="https://github.com/SteelCrab.png?size=96"', body)
        self.assertIn('src="https://github.com/kudala-bharani.png?size=96"', body)
        self.assertIn('href="https://github.com/SteelCrab"', body)
        self.assertNotIn("- SteelCrab", body)
        self.assertGreater(body.rfind("## Contributors"), body.rfind("## Changelog"))
        self.assertTrue(body.rstrip().endswith("</p>"))

    def test_skips_bot_contributors(self) -> None:
        names = notes.filter_contributors(["SteelCrab", "dependabot[bot]", "github-actions[bot]"])
        self.assertEqual(names, ["SteelCrab"])

    def test_writes_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "notes.md"
            notes.write_notes(
                tag="v0.1.0",
                changelog_text=CHANGELOG,
                contributors=["SteelCrab"],
                repo="SteelCrab/firecrab",
                dest=out,
            )
            text = out.read_text(encoding="utf-8")
            self.assertTrue(text.startswith("# firecrab 0.1.0\n"))


if __name__ == "__main__":
    unittest.main()
