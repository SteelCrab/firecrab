import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_clippy_warnings as gate


def compiler_message(
    package_id,
    lint,
    text,
    *,
    file_name="src/main.rs",
    line=1,
    target="crate",
    level="warning",
):
    return {
        "reason": "compiler-message",
        "package_id": package_id,
        "target": {"name": target},
        "message": {
            "level": level,
            "message": text,
            "code": {"code": lint} if lint else None,
            "spans": [
                {
                    "file_name": file_name,
                    "line_start": line,
                    "line_end": line,
                    "column_start": 1,
                    "column_end": 2,
                    "is_primary": True,
                }
            ],
        },
    }


class ClippyWarningGateTests(unittest.TestCase):
    def write_messages(self, messages):
        handle = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", delete=False)
        self.addCleanup(Path(handle.name).unlink, missing_ok=True)
        with handle:
            for message in messages:
                json.dump(message, handle)
                handle.write("\n")
        return handle.name

    def test_collect_deduplicates_the_same_warning_across_all_targets(self):
        warning = compiler_message(
            "path+file:///repo/firecrab-api#0.1.1",
            "dead_code",
            "function is never used",
            file_name="firecrab-api/src/main.rs",
            line=20,
            target="firecrab-api",
        )
        path = self.write_messages(
            [
                warning,
                warning,
                {"reason": "build-finished", "success": True},
                compiler_message(
                    "path+file:///repo/firecrab-api#0.1.1",
                    "dead_code",
                    "not a warning",
                    level="note",
                ),
            ]
        )

        counts = gate.collect(path)

        self.assertEqual(counts["total"], 1)
        self.assertEqual(counts["packages"], {"firecrab-api": 1})
        self.assertEqual(counts["lints"], {"dead_code": 1})

    def test_collect_aggregates_by_crate_and_lint(self):
        path = self.write_messages(
            [
                compiler_message(
                    "path+file:///repo/firecrab-api#0.1.1",
                    "dead_code",
                    "one",
                    line=1,
                    target="firecrab-api",
                ),
                compiler_message(
                    "path+file:///repo/firecrab-api#0.1.1",
                    "clippy::collapsible_if",
                    "two",
                    line=2,
                    target="firecrab-api",
                ),
                compiler_message(
                    "path+file:///repo/firecrab-cli#0.1.1",
                    "clippy::collapsible_if",
                    "three",
                    line=3,
                    target="firecrab",
                ),
            ]
        )

        counts = gate.collect(path)

        self.assertEqual(counts["total"], 3)
        self.assertEqual(
            counts["packages"], {"firecrab-api": 2, "firecrab-cli": 1}
        )
        self.assertEqual(
            counts["lints"], {"clippy::collapsible_if": 2, "dead_code": 1}
        )

    def test_package_name_handles_current_registry_path_and_legacy_ids(self):
        self.assertEqual(
            gate.package_name("path+file:///repo/firecrab-api#0.1.1"),
            "firecrab-api",
        )
        self.assertEqual(
            gate.package_name(
                "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.228"
            ),
            "serde",
        )
        self.assertEqual(
            gate.package_name("firecrab-cli 0.1.1 (path+file:///repo/firecrab-cli)"),
            "firecrab-cli",
        )

    def test_compare_rejects_an_increase_and_a_new_lint(self):
        baseline = {
            "total": 1,
            "packages": {"firecrab-api": 1},
            "lints": {"dead_code": 1},
        }
        current = {
            "total": 2,
            "packages": {"firecrab-api": 2},
            "lints": {"dead_code": 1, "unused_imports": 1},
        }

        regressions, stale = gate.compare(current, baseline)

        self.assertFalse(stale)
        self.assertTrue(any("total warnings increased" in item for item in regressions))
        self.assertTrue(any("unused_imports" in item for item in regressions))

    def test_compare_rejects_stale_baseline_after_warning_cleanup(self):
        baseline = {
            "total": 2,
            "packages": {"firecrab-api": 2},
            "lints": {"dead_code": 2},
        }
        current = {
            "total": 1,
            "packages": {"firecrab-api": 1},
            "lints": {"dead_code": 1},
        }

        regressions, stale = gate.compare(current, baseline)

        self.assertFalse(regressions)
        self.assertTrue(any("total warnings decreased" in item for item in stale))

    def test_write_baseline_is_sorted_and_reviewable(self):
        current = {
            "total": 2,
            "packages": {"z-crate": 1, "a-crate": 1},
            "lints": {"unused_imports": 1, "dead_code": 1},
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "baseline.json"
            gate.write_baseline(path, current)
            written = path.read_text(encoding="utf-8")

        self.assertLess(written.index('"a-crate"'), written.index('"z-crate"'))
        self.assertLess(written.index('"dead_code"'), written.index('"unused_imports"'))

    def test_summary_shows_current_baseline_and_delta_tables(self):
        baseline = {
            "total": 1,
            "packages": {"firecrab-api": 1},
            "lints": {"dead_code": 1},
        }
        summary = gate.summary_markdown(baseline, baseline, [], [])

        self.assertIn("| Crate | Baseline | Current | Δ |", summary)
        self.assertIn("| Lint | Baseline | Current | Δ |", summary)
        self.assertIn("baseline unchanged", summary)


if __name__ == "__main__":
    unittest.main()
