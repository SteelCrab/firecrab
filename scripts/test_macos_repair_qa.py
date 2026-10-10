"""Guard and rollback contracts; these tests never call the user's service."""

import importlib.util
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("repair_qa", Path(__file__).with_name("ci-qa-macos-repair.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class RepairQA(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name).resolve()
        self.home, self.install = self.root / "micromanager", self.root / "bin"
        self.addCleanup(self.directory.cleanup)

    def assets(self):
        for path in [self.install / "firecrab", self.install / "firecrab-micromanager-macos"] + [
            self.home / file for file in (
                "system/debian-system.raw", "data/firecrab-data.raw", "system/Image", "system/initrd.img",
                "runtime/manager_ed25519", "runtime/provisioned", "runtime/known_hosts", "downloads/archive",
            )
        ]:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("preserved\n")

    def test_running_transitional_and_unknown_vms_block_mutation(self):
        for state in ("running", "starting", "stopping", "unknown", None):
            with self.assertRaises(qa.QAError):
                qa.require_idle([{"id": "vm", "state": state}])
        qa.require_idle([])
        qa.require_idle([{"state": state} for state in ("created", "stopped", "error")])

    def test_disk_replacement_binary_change_and_host_key_removal_are_detected(self):
        self.assets()
        baseline = qa.snapshot(self.home, self.install)
        qa.assert_preserved(baseline, qa.snapshot(self.home, self.install))
        disk = self.home / "data/firecrab-data.raw"
        disk.rename(disk.with_suffix(".backup"))
        disk.write_text("preserved\n")
        with self.assertRaisesRegex(qa.QAError, "disks"):
            qa.assert_preserved(baseline, qa.snapshot(self.home, self.install))
        disk.unlink()
        disk.with_suffix(".backup").rename(disk)
        (self.install / "firecrab").write_text("replaced")
        with self.assertRaisesRegex(qa.QAError, "hashes"):
            qa.assert_preserved(baseline, qa.snapshot(self.home, self.install))
        (self.install / "firecrab").write_text("preserved\n")
        (self.home / "runtime/known_hosts").write_text("")
        with self.assertRaisesRegex(qa.QAError, "host keys"):
            qa.assert_preserved(baseline, qa.snapshot(self.home, self.install))

    def test_boot_writes_and_new_host_pins_do_not_count_as_disk_replacement(self):
        self.assets()
        baseline = qa.snapshot(self.home, self.install)
        (self.home / "system/debian-system.raw").write_text("bootwrite\n")
        (self.home / "runtime/known_hosts").write_text("preserved\nnew host\n")
        qa.assert_preserved(baseline, qa.snapshot(self.home, self.install))

    def test_failed_gate_never_stops_or_restores_service(self):
        runner = qa.QA("cli", self.home, self.install, self.root / "results")
        with patch.object(runner, "gate", side_effect=qa.QAError("active VM")), \
                patch.object(runner, "service") as service, patch.object(runner, "restore") as restore, \
                redirect_stdout(io.StringIO()):
            self.assertEqual(runner.execute(), 1)
            service.assert_not_called()
            restore.assert_not_called()
        summary = json.loads((runner.results / "summary.json").read_text())
        self.assertEqual(summary["status"], "FAILED")
        self.assertEqual(summary["checks"][0]["status"], "FAILED")

    def test_failed_repair_restores_registration_and_records_failure(self):
        runner = qa.QA("cli", self.home, self.install, self.root / "results")
        wrapper = self.root / "daemon.sh"
        backup = self.root / "backup"
        wrapper.write_text("damaged")
        backup.write_text("original")
        runner.backups[wrapper] = backup

        def fail():
            runner.mutated = True
            raise qa.QAError("repair failed")

        with patch.object(runner, "gate"), patch.object(runner, "restart", side_effect=fail), \
                patch.object(runner, "service") as service, patch.object(runner, "api"), \
                redirect_stdout(io.StringIO()):
            self.assertEqual(runner.execute(), 1)
        self.assertEqual(wrapper.read_text(), "original")
        self.assertEqual([call.args[0] for call in service.call_args_list], ["stop", "start", "status"])
        self.assertEqual(runner.summary["restoration"], "PASS")
        self.assertEqual(runner.summary["status"], "FAILED")

    def test_interrupted_qa_still_restores_the_service(self):
        runner = qa.QA("cli", self.home, self.install, self.root / "results")

        def interrupt():
            runner.mutated = True
            raise KeyboardInterrupt()

        with patch.object(runner, "gate"), patch.object(runner, "restart", side_effect=interrupt), \
                patch.object(runner, "restore") as restore, redirect_stdout(io.StringIO()):
            self.assertEqual(runner.execute(), 1)
            restore.assert_called_once()
        self.assertEqual(runner.summary["error"], "interrupted")


if __name__ == "__main__":
    unittest.main()
