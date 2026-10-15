"""Safety and helper-contract tests; these tests never run the helper or a VM."""

import importlib.util
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "macos_os_repair_qa", Path(__file__).with_name("ci-qa-macos-os-repair.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class OsRepairQA(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name).resolve()
        self.user_home = self.root / "user"
        self.user_home.mkdir()
        self.fixture = self.root / "fixture"
        self.fixture.mkdir()
        self.results = self.root / "results"
        self.helper = self.root / "signed-helper"
        self.helper.write_text("helper placeholder\n")
        self.helper.chmod(0o700)
        self.addCleanup(self.directory.cleanup)

    def assets(self, *, reference=False):
        files = {
            qa.FIXTURE_MARKER: qa.FIXTURE_MARKER_CONTENT.encode(),
            "system/debian-system.raw": b"original-os-disk\n",
            "system/Image": b"kernel\n",
            "system/initrd.img": b"initrd\n",
            "data/firecrab-data.raw": b"data-disk\n",
            "runtime/manager_ed25519": b"private-key-placeholder\n",
            "runtime/manager_ed25519.pub": b"public-key-placeholder\n",
            "runtime/known_hosts": b"pinned-host\n",
        }
        for relative, content in files.items():
            path = self.fixture / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        self.stage = self.fixture / "runtime/repairs/run-001/debian-system.repaired.raw"
        self.stage.parent.mkdir(parents=True, exist_ok=True)
        self.stage.write_bytes(b"staged-os-disk\n")
        self.reference = self.stage.with_name(qa.REFERENCE_IMAGE_NAME)
        if reference:
            self.reference.write_bytes(b"reference-os-disk\n")
        return self.stage

    def new_qa(self, *, environ=None, expect_restored_files=None):
        return qa.QA(
            self.helper, self.fixture, self.results, user_home=self.user_home,
            environ={} if environ is None else environ,
            expect_restored_files=expect_restored_files,
        )

    def test_clean_helper_result_matches_contract(self):
        result = qa.validate_helper_result(
            b'{"check_exit":0,"repair_exit":null,"verify_exit":0,"changed":false}\n', 0)
        self.assertEqual(result["check_exit"], 0)
        self.assertIsNone(result["repair_exit"])
        self.assertFalse(result["changed"])
        self.assertEqual(result["restored_files"], 0)

    def test_restored_files_drive_changed_and_optional_expected_count(self):
        clean_check_with_restore = {
            "check_exit": 0, "repair_exit": None, "verify_exit": 0,
            "changed": True, "restored_files": 2,
        }
        result = qa.validate_helper_result(json.dumps(clean_check_with_restore), 0, 2)
        self.assertEqual(result["restored_files"], 2)
        with self.assertRaisesRegex(qa.QAError, "expected 3 restored files"):
            qa.validate_helper_result(json.dumps(clean_check_with_restore), 0, 3)

        repaired = {
            "check_exit": 4, "repair_exit": 1, "verify_exit": 0,
            "changed": True, "restored_files": 1,
        }
        self.assertEqual(qa.validate_helper_result(json.dumps(repaired), 0)["restored_files"], 1)

    def test_repair_exit_codes_zero_through_three_are_accepted(self):
        for check_exit in range(1, 8):
            for code in range(4):
                with self.subTest(check_exit=check_exit, repair_exit=code):
                    output = json.dumps({
                        "check_exit": check_exit, "repair_exit": code,
                        "verify_exit": 0, "changed": True,
                    })
                    result = qa.validate_helper_result(output, 0)
                    self.assertEqual(result["check_exit"], check_exit)
                    self.assertEqual(result["repair_exit"], code)

    def test_helper_exit_verify_exit_and_json_shape_are_required(self):
        clean = {"check_exit": 0, "repair_exit": None, "verify_exit": 0, "changed": False}
        with self.assertRaisesRegex(qa.QAError, "helper exited"):
            qa.validate_helper_result(json.dumps(clean), 7)
        for update, message in (
            ({"verify_exit": 1}, "verification failed"),
            ({"repair_exit": 4}, "repair_exit"),
            ({"changed": True}, "changed must report"),
            ({"extra": 1}, "fields differ"),
            ({"check_exit": True}, "check_exit"),
            ({"check_exit": 8}, "check_exit"),
            ({"restored_files": -1}, "nonnegative integer"),
            ({"restored_files": True}, "nonnegative integer"),
        ):
            with self.subTest(update=update):
                result = dict(clean)
                result.update(update)
                with self.assertRaisesRegex(qa.QAError, message):
                    qa.validate_helper_result(json.dumps(result), 0)
        with self.assertRaisesRegex(qa.QAError, "one JSON object"):
            qa.validate_helper_result(json.dumps(clean) + "\nextra\n", 0)

    def test_uncorrected_check_failure_cannot_claim_clean(self):
        result = {"check_exit": 4, "repair_exit": None, "verify_exit": 0, "changed": False}
        with self.assertRaisesRegex(qa.QAError, "requires a repair exit"):
            qa.validate_helper_result(json.dumps(result), 0)

    def test_restored_files_contract_rejects_inconsistent_results(self):
        cases = [
            ({"check_exit": 0, "repair_exit": 1, "verify_exit": 0,
              "changed": True, "restored_files": 0}, "skip filesystem repair"),
            ({"check_exit": 4, "repair_exit": None, "verify_exit": 0,
              "changed": True, "restored_files": 1}, "requires a repair exit"),
            ({"check_exit": 0, "repair_exit": None, "verify_exit": 0,
              "changed": False, "restored_files": 1}, "changed must report"),
        ]
        for result, message in cases:
            with self.subTest(result=result), self.assertRaisesRegex(qa.QAError, message):
                qa.validate_helper_result(json.dumps(result), 0)

    def test_fixture_rejects_default_and_configured_managed_homes(self):
        default = self.user_home / qa.DEFAULT_MANAGED_SUFFIX
        default.mkdir(parents=True)
        with self.assertRaisesRegex(qa.QAError, "managed installation home"):
            qa.validate_fixture_home(default, self.user_home, {})

        configured = self.root / "installed-home"
        configured.mkdir()
        with self.assertRaisesRegex(qa.QAError, "managed installation home"):
            qa.validate_fixture_home(self.fixture, self.user_home,
                                     {"FIRECRAB_MICROMANAGER_HOME": str(self.fixture)})
        with self.assertRaisesRegex(qa.QAError, "managed installation home"):
            qa.validate_fixture_home(configured, self.user_home,
                                     {"FIRECRAB_MICROMANAGER_HOME": str(configured)})

    def test_fixture_and_helper_symlink_paths_are_rejected(self):
        fixture_link = self.root / "fixture-link"
        fixture_link.symlink_to(self.fixture)
        with self.assertRaisesRegex(qa.QAError, "symlinked path"):
            qa.validate_fixture_home(fixture_link, self.user_home, {})
        helper_link = self.root / "helper-link"
        helper_link.symlink_to(self.helper)
        with self.assertRaisesRegex(qa.QAError, "symlinked path"):
            qa.validate_helper(helper_link)

    def test_fixture_marker_is_required_and_exact(self):
        self.assets()
        marker = self.fixture / qa.FIXTURE_MARKER
        qa.require_fixture_marker(self.fixture)
        marker.write_text("not an isolated fixture\n")
        with self.assertRaisesRegex(qa.QAError, "unexpected content"):
            qa.require_fixture_marker(self.fixture)
        marker.unlink()
        with self.assertRaisesRegex(qa.QAError, "required fixture file is missing"):
            qa.require_fixture_marker(self.fixture)

    def test_staging_image_must_be_unique_and_unlinked(self):
        stage = self.assets()
        self.assertEqual(qa.select_staged_image(self.fixture), stage)
        second = self.fixture / "runtime/repairs/run-002/debian-system.repaired.raw"
        second.parent.mkdir()
        second.write_bytes(b"second stage")
        with self.assertRaisesRegex(qa.QAError, "exactly one"):
            qa.select_staged_image(self.fixture)
        second.unlink()
        stage.unlink()
        os.link(self.fixture / "system/debian-system.raw", stage)
        with self.assertRaisesRegex(qa.QAError, "hard links"):
            qa.select_staged_image(self.fixture)

    def test_staging_symlink_to_original_is_rejected(self):
        stage = self.assets()
        stage.unlink()
        stage.symlink_to(self.fixture / "system/debian-system.raw")
        with self.assertRaisesRegex(qa.QAError, "symlinked path"):
            qa.select_staged_image(self.fixture)

    def test_reference_is_optional_and_uses_the_stage_sibling_name(self):
        stage = self.assets()
        self.assertIsNone(qa.select_reference_image(stage, self.fixture, self.user_home, {}))
        reference = stage.with_name(qa.REFERENCE_IMAGE_NAME)
        reference.write_bytes(b"private reference image\n")
        selected = qa.select_reference_image(stage, self.fixture, self.user_home, {})
        self.assertEqual(selected, reference)

    def test_reference_symlink_and_hardlink_aliases_are_rejected(self):
        stage = self.assets()
        reference = stage.with_name(qa.REFERENCE_IMAGE_NAME)
        reference.symlink_to(self.fixture / "system/debian-system.raw")
        with self.assertRaisesRegex(qa.QAError, "symlinked path"):
            qa.select_reference_image(stage, self.fixture, self.user_home, {})
        reference.unlink()

        for protected in (stage, self.fixture / "system/debian-system.raw",
                          self.fixture / "data/firecrab-data.raw"):
            with self.subTest(protected=protected):
                os.link(protected, reference)
                with self.assertRaisesRegex(qa.QAError, "hard links"):
                    qa.select_reference_image(stage, self.fixture, self.user_home, {})
                reference.unlink()

    def test_reference_is_captured_and_must_remain_unchanged(self):
        stage = self.assets(reference=True)
        selected = qa.select_reference_image(stage, self.fixture, self.user_home, {})
        before = qa.capture_preservation(self.fixture, stage, selected)
        qa.require_snapshot_complete(before)
        self.assertIn("reference_os_disk", before["protected"])
        self.assertIsInstance(before["protected"]["reference_os_disk"], dict)

        changed_reference = selected.with_name("reference-replacement.raw")
        changed_reference.write_bytes(selected.read_bytes())
        changed_reference.replace(selected)
        after = qa.capture_preservation(self.fixture, stage, selected)
        with self.assertRaisesRegex(qa.QAError, "changed or replaced"):
            qa.require_protected_preserved(before, after)

    def test_staged_image_cannot_alias_protected_or_default_home_assets(self):
        stage = self.assets()
        actual = self.user_home / qa.DEFAULT_MANAGED_SUFFIX
        (actual / "system").mkdir(parents=True)
        (actual / "system/debian-system.raw").write_bytes(b"different file")
        qa.assert_staged_isolated(stage, self.fixture, self.user_home, {})

        stage.unlink()
        os.link(actual / "system/debian-system.raw", stage)
        with self.assertRaisesRegex(qa.QAError, "hard links"):
            qa.assert_staged_isolated(stage, self.fixture, self.user_home, {})

        # A symlink in place of the staged image is rejected before it can reach a helper.
        stage.unlink()
        stage.symlink_to(actual / "system/debian-system.raw")
        with self.assertRaisesRegex(qa.QAError, "symlinked path"):
            qa.assert_staged_isolated(stage, self.fixture, self.user_home, {})

    def test_active_staging_image_blocks_preflight(self):
        self.assets()
        result = SimpleNamespace(returncode=0, stdout="4242\n", stderr="")
        with patch.object(qa.shutil, "which", return_value="/usr/sbin/lsof"), \
                patch.object(qa.subprocess, "run", return_value=result) as runner:
            with self.assertRaisesRegex(qa.QAError, "open by process"):
                qa.require_image_idle(self.stage)
        runner.assert_called_once_with(
            ["/usr/sbin/lsof", "-t", str(self.stage)],
            capture_output=True, text=True, timeout=15,
        )

    def test_lsof_uncertainty_fails_closed(self):
        self.assets()
        result = SimpleNamespace(returncode=1, stdout="", stderr="permission denied")
        with patch.object(qa.shutil, "which", return_value="/usr/sbin/lsof"), \
                patch.object(qa.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(qa.QAError, "could not prove"):
                qa.require_image_idle(self.stage)

    def test_preservation_snapshots_allow_staging_changes_only(self):
        stage = self.assets()
        before = qa.capture_preservation(self.fixture, stage)
        stage.write_bytes(b"repaired staged disk\n")
        after = qa.capture_preservation(self.fixture, stage)
        qa.require_protected_preserved(before, after)
        self.assertNotEqual(before["staged_os_disk"]["sha256"],
                            after["staged_os_disk"]["sha256"])

        data = self.fixture / "data/firecrab-data.raw"
        replacement = data.with_name("data-replacement")
        replacement.write_bytes(data.read_bytes())
        replacement.replace(data)
        changed = qa.capture_preservation(self.fixture, stage)
        with self.assertRaisesRegex(qa.QAError, "changed or replaced"):
            qa.require_protected_preserved(after, changed)

    def test_staged_image_must_keep_its_inode_and_single_link(self):
        stage = self.assets()
        before = qa.capture_preservation(self.fixture, stage)
        replacement = stage.with_name("replacement.raw")
        replacement.write_bytes(b"new staged inode\n")
        replacement.replace(stage)
        replaced = qa.capture_preservation(self.fixture, stage)
        with self.assertRaisesRegex(qa.QAError, "replaced the staged OS image"):
            qa.require_protected_preserved(before, replaced)

        stage.write_bytes(b"restored in-place stage\n")
        before = qa.capture_preservation(self.fixture, stage)
        alias = stage.with_name("stage-alias.raw")
        os.link(stage, alias)
        linked = qa.capture_preservation(self.fixture, stage)
        with self.assertRaisesRegex(qa.QAError, "single-link regular file"):
            qa.require_protected_preserved(before, linked)

    def test_optional_ssh_pin_creation_is_a_preservation_change(self):
        stage = self.assets()
        pins = self.fixture / "runtime/known_hosts"
        pins.unlink()
        before = qa.capture_preservation(self.fixture, stage)
        self.assertIsNone(before["protected"]["ssh_known_hosts"])
        qa.require_snapshot_complete(before)
        pins.write_text("new pin\n")
        after = qa.capture_preservation(self.fixture, stage)
        with self.assertRaisesRegex(qa.QAError, "changed or replaced"):
            qa.require_protected_preserved(before, after)

    def test_failed_preservation_still_records_partial_after_snapshot(self):
        stage = self.assets()
        runner = self.new_qa()
        self.addCleanup(runner.log.close)
        runner.fixture_home = self.fixture
        runner.staged = stage
        runner.before = runner.capture()
        runner.record_snapshot("preservation-before.json", runner.before)
        (self.fixture / "data/firecrab-data.raw").unlink()
        runner.record_preservation()
        self.assertEqual(runner.summary["preservation"], "FAILED")
        after = json.loads((runner.results / "preservation-after.json").read_text())
        self.assertIn("error", after["protected"]["data_disk"])

    def test_helper_invocation_uses_absolute_stage_and_fixture_environment(self):
        self.assets()
        runner = self.new_qa()
        self.addCleanup(runner.log.close)
        runner.helper = self.helper
        runner.fixture_home = self.fixture
        runner.staged = self.stage
        payload = {"check_exit": 4, "repair_exit": 1, "verify_exit": 0, "changed": True}

        class FakeProcess:
            pid = 991
            returncode = 0

            def communicate(self, timeout):
                self.timeout = timeout
                return (json.dumps(payload).encode(), b"console details\n")

        process = FakeProcess()
        with patch.object(qa.subprocess, "Popen", return_value=process) as popen, \
                patch.object(qa, "require_image_idle"):
            result = runner.invoke_helper()
        args, kwargs = popen.call_args
        self.assertEqual(args[0], [str(self.helper), "repair-os", str(self.stage)])
        self.assertEqual(kwargs["env"]["FIRECRAB_MICROMANAGER_HOME"], str(self.fixture))
        self.assertEqual(kwargs["cwd"], self.fixture)
        self.assertTrue(kwargs["start_new_session"])
        self.assertEqual(process.timeout, qa.HELPER_TIMEOUT)
        self.assertEqual(result, {**payload, "restored_files": 0})
        self.assertEqual(runner.summary["helper_exit"], 0)
        self.assertEqual((runner.results / "helper.stderr.log").read_bytes(), b"console details\n")

    def test_helper_receives_private_reference_and_expected_restore_count(self):
        self.assets(reference=True)
        runner = self.new_qa(expect_restored_files=2)
        self.addCleanup(runner.log.close)
        runner.helper = self.helper
        runner.fixture_home = self.fixture
        runner.staged = self.stage
        runner.reference = self.reference
        payload = {
            "check_exit": 0, "repair_exit": None, "verify_exit": 0,
            "changed": True, "restored_files": 2,
        }

        class FakeProcess:
            pid = 993
            returncode = 0

            def communicate(self, timeout):
                self.timeout = timeout
                return (json.dumps(payload).encode(), b"reference used\n")

        with patch.object(qa.subprocess, "Popen", return_value=FakeProcess()) as popen, \
                patch.object(qa, "require_image_idle"):
            result = runner.invoke_helper()
        args, kwargs = popen.call_args
        self.assertEqual(args[0], [
            str(self.helper), "repair-os", str(self.stage),
            "--reference", str(self.reference),
        ])
        self.assertEqual(kwargs["env"]["FIRECRAB_MICROMANAGER_HOME"], str(self.fixture))
        self.assertEqual(result["restored_files"], 2)

    def test_timeout_records_when_staged_image_remains_active(self):
        self.assets()
        runner = self.new_qa()
        self.addCleanup(runner.log.close)
        runner.helper = self.helper
        runner.fixture_home = self.fixture
        runner.staged = self.stage

        class TimedOutProcess:
            pid = 992
            returncode = -2

            def communicate(self, timeout):
                raise qa.subprocess.TimeoutExpired(
                    ["repair-os"], timeout, output=b"partial stdout", stderr=b"console\n")

        process = TimedOutProcess()
        with patch.object(qa.subprocess, "Popen", return_value=process), \
                patch.object(runner, "stop_child",
                             return_value=(b"partial stdout", b"console\n")), \
                patch.object(qa, "require_image_idle",
                             side_effect=qa.QAError("fixture image remains open by process 992")):
            with self.assertRaisesRegex(qa.QAError, "exceeded .*post-helper image idle check failed"):
                runner.invoke_helper()

        self.assertTrue(runner.summary["helper_timed_out"])
        self.assertEqual(runner.summary["staged_image_idle_after"]["status"], "FAILED")
        self.assertIn("remains open", runner.summary["staged_image_idle_after"]["error"])
        self.assertEqual((runner.results / "helper.stdout").read_bytes(), b"partial stdout")
        self.assertEqual((runner.results / "helper.stderr.log").read_bytes(), b"console\n")

    def test_interrupted_helper_run_keeps_before_after_evidence(self):
        stage = self.assets()
        runner = self.new_qa()
        self.addCleanup(runner.log.close)
        runner.fixture_home = self.fixture
        runner.staged = stage
        with patch.object(runner, "preflight"), \
                patch.object(runner, "invoke_helper", side_effect=KeyboardInterrupt), \
                redirect_stdout(io.StringIO()):
            self.assertEqual(runner.execute(), 130)
        self.assertTrue((runner.results / "preservation-before.json").is_file())
        self.assertTrue((runner.results / "preservation-after.json").is_file())
        summary = json.loads((runner.results / "summary.json").read_text())
        self.assertEqual(summary["status"], "FAILED")
        self.assertEqual(summary["error"], "interrupted")
        self.assertEqual(summary["preservation"], "PASS")


if __name__ == "__main__":
    unittest.main()
