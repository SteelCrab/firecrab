#!/usr/bin/env python3
"""Unit tests for scripts/check_changelog.py (stdlib unittest)."""

from __future__ import annotations

import tempfile
import unittest
import importlib.util
from pathlib import Path
from textwrap import dedent


def _load_checker():
    path = Path(__file__).with_name("check-changelog.py")
    spec = importlib.util.spec_from_file_location("check_changelog", path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check_changelog = _load_checker()

VALID = dedent(
    """\
    # Changelog

    ## [0.1.0] - 2026-08-16

    ### Added

    - First public release.

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


class ValidateChangelogTests(unittest.TestCase):
    def test_valid_document_has_no_problems(self) -> None:
        self.assertEqual(check_changelog.validate_text(VALID, "0.1.0"), [])

    def test_missing_required_section_is_reported(self) -> None:
        text = VALID.replace("### Improved\n\n- None.\n", "")
        problems = check_changelog.validate_text(text, "0.1.0")
        self.assertTrue(any("Improved" in problem for problem in problems), problems)

    def test_version_heading_must_match_workspace(self) -> None:
        problems = check_changelog.validate_text(VALID, "0.2.0")
        self.assertTrue(any("0.2.0" in problem for problem in problems), problems)

    def test_extract_returns_the_named_release_body(self) -> None:
        body = check_changelog.extract_notes(VALID, "v0.1.0")
        self.assertIn("### Added", body)
        self.assertIn("First public release.", body)
        self.assertTrue(body.startswith("## [0.1.0] - 2026-08-16"))

    def test_extract_unknown_tag_raises(self) -> None:
        with self.assertRaises(check_changelog.ChangelogError):
            check_changelog.extract_notes(VALID, "v9.9.9")

    def test_repo_changelog_matches_workspace_version(self) -> None:
        repo = Path(__file__).resolve().parent.parent
        version = check_changelog.workspace_version(repo / "Cargo.toml")
        problems = check_changelog.validate_path(repo / "CHANGELOG.md", version)
        self.assertEqual(problems, [], problems)


class FileHelperTests(unittest.TestCase):
    def test_validate_path_missing_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "CHANGELOG.md"
            problems = check_changelog.validate_path(path, "0.1.0")
        self.assertTrue(any("missing" in problem.lower() for problem in problems), problems)


if __name__ == "__main__":
    unittest.main()
