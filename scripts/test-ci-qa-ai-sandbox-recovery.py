#!/usr/bin/env python3
"""Offline recovery-gate guard tests. These do not establish live acceptance."""
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import uuid

spec = importlib.util.spec_from_file_location("recovery", Path(__file__).with_name("ci-qa-ai-sandbox-recovery.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
RUN, CONNECTION, VM, NETWORK, PROJECT = [str(uuid.uuid4()) for _ in range(5)]
NONCE = "a" * 32
LABEL = f"sandbox-recovery-{RUN}"


class Api:
    base = "http://127.0.0.1:5523"

    def __init__(self):
        self.calls = []
        self.accounts = [{"id": CONNECTION, "name": LABEL}]
        self.vms = [{"id": VM, "name": LABEL, "state": "running", "purpose": "instance", "microNetworkId": NETWORK, "egressPolicy": "isolated"}]
        self.network = {"id": NETWORK, "name": LABEL, "internetEnabled": False}
        self.projects = []

    def request(self, method, path, body=None, allowed=(200,), timeout=360):
        self.calls.append((method, path, body))
        if method == "GET":
            return 200, {"/api/pipe/ai/connections": self.accounts, "/api/vms": self.vms,
                         f"/api/micro-networks/{NETWORK}": self.network, "/api/ai-sandbox/projects": self.projects}[path]
        if method == "PUT":
            return 200, body
        raise qa.qa.QaFailure("Deliberate validation stop")


def result():
    script = qa.command(RUN, "cancel", 90)
    return {"projectId": PROJECT, "selection": {"connectionId": CONNECTION, "model": "verified"},
            "runtime": {"vmId": VM}, "state": "cancelled",
            "events": [{"name": "run_command", "input": json.dumps({"command": script}),
                        "output": f"RECOVERY414:{RUN}:cancel:stdout:{NONCE}\n",
                        "stderr": f"RECOVERY414:{RUN}:cancel:stderr\n", "exitCode": None}],
            "changes": [{"path": "executions.txt", "after": "once\n", "applied": False},
                        {"path": "nonce", "after": NONCE, "applied": False}]}


class RecoveryTests(unittest.TestCase):
    def test_ownership_marker_requires_private_root_file_and_controlled_directory(self):
        path = Mock()
        path.parent.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFDIR | 0o755, st_uid=0)
        path.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFREG | 0o600, st_uid=0)
        path.read_text.return_value = RUN + "\n"
        qa.owner_marker(path, RUN)
        for mode, uid in [(stat.S_IFLNK | 0o600, 0), (stat.S_IFREG | 0o644, 0), (stat.S_IFREG | 0o600, 501)]:
            path.lstat.return_value = SimpleNamespace(st_mode=mode, st_uid=uid)
            with self.assertRaises(qa.qa.QaFailure):
                qa.owner_marker(path, RUN)
        path.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFREG | 0o600, st_uid=0)
        path.parent.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFDIR | 0o777, st_uid=0)
        with self.assertRaises(qa.qa.QaFailure):
            qa.owner_marker(path, RUN)
        path.parent.lstat.return_value = SimpleNamespace(st_mode=stat.S_IFDIR | 0o755, st_uid=0)
        path.read_text.return_value = str(uuid.uuid4())
        with self.assertRaises(qa.qa.QaFailure):
            qa.owner_marker(path, RUN)

    def test_shared_or_foreign_resources_are_rejected_before_any_write_or_controller_access(self):
        for changed in ("account", "extraAccount", "vm", "extraVm", "stoppedVm", "internalVm", "network", "project", "egress", "internet", "ipv6"):
            api = Api()
            if changed == "account": api.accounts[0]["name"] = "borrowed"
            if changed == "extraAccount": api.accounts.append({"id": str(uuid.uuid4())})
            if changed == "vm": api.vms[0]["name"] = "borrowed"
            if changed == "extraVm": api.vms.append({"id": str(uuid.uuid4())})
            if changed == "stoppedVm": api.vms[0]["state"] = "stopped"
            if changed == "internalVm": api.vms[0]["purpose"] = "image"
            if changed == "network": api.network["name"] = "borrowed"
            if changed == "project": api.projects.append({"id": PROJECT})
            if changed == "egress": api.vms[0]["egressPolicy"] = "internet"
            if changed == "internet": api.network["internetEnabled"] = True
            if changed == "ipv6": api.network["ipv6Cidr"] = "fd00::/64"
            with self.subTest(changed=changed), patch.object(qa, "control") as control:
                with self.assertRaises(qa.qa.QaFailure):
                    qa.owned(api, RUN, CONNECTION, VM)
                control.assert_not_called()
                self.assertTrue(all(method == "GET" for method, _, _ in api.calls))

    def test_owns_exactly_one_api_vm_and_network(self):
        label, vm, network = qa.owned(Api(), RUN, CONNECTION, VM)
        self.assertEqual((label, vm["id"], network["id"]), (LABEL, VM, NETWORK))

    def test_controller_pid_must_own_the_selected_loopback_listener(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            process = root / "42"
            (process / "fd").mkdir(parents=True)
            executable = root / "firecrab-api"
            executable.touch()
            (process / "exe").symlink_to(executable)
            (process / "fd/3").symlink_to("socket:[123]")
            (root / "net").mkdir()
            tcp = root / "net/tcp"
            tcp.write_text("header\n0: 0100007F:1593 00000000:0000 0A 0 0 0 0 0 123\n")
            def proc(path): return root / str(path).removeprefix("/proc/")
            with patch.object(qa, "control", return_value="42"), patch.object(qa, "Path", side_effect=proc):
                self.assertEqual(qa.controller_pid(Api(), "firecrab-api.service"), "42")
                tcp.write_text("header\n0: 0100007F:1593 00000000:0000 0A 0 0 0 0 0 456\n")
                with self.assertRaises(qa.qa.QaFailure):
                    qa.controller_pid(Api(), "firecrab-api.service")
                tcp.write_text("header\n0: 00000000:1593 00000000:0000 0A 0 0 0 0 0 123\n")
                with self.assertRaises(qa.qa.QaFailure):
                    qa.controller_pid(Api(), "firecrab-api.service")

    def test_zero_or_non_numeric_controller_pid_cannot_trigger_a_fault(self):
        for value in ("0", "1", "invalid", ""):
            with patch.object(qa, "control", return_value=value), self.assertRaises(qa.qa.QaFailure):
                qa.controller_pid(Api(), "firecrab-api.service")

    def test_partial_reboot_output_keeps_unknown_exit_without_inventing_success(self):
        task = result()
        task["events"][0]["stderr"] += "Killed\n"
        self.assertEqual(qa.validate(task, PROJECT, VM, task["selection"], qa.command(RUN, "cancel", 90),
                                     RUN, "cancel", "cancelled", NONCE), NONCE)
        self.assertIsNone(task["events"][0]["exitCode"])

    def test_replay_forged_output_lost_stderr_and_wrong_references_fail_validation(self):
        for changed in ("replay", "stdout", "stderr", "project", "vm", "selection", "command", "applied"):
            task = result()
            selection = dict(task["selection"])
            if changed == "replay": task["changes"][0]["after"] = "once\nonce\n"
            if changed == "stdout": task["events"][0]["output"] = "hallucinated"
            if changed == "stderr": task["events"][0]["stderr"] = ""
            if changed == "project": task["projectId"] = str(uuid.uuid4())
            if changed == "vm": task["runtime"]["vmId"] = str(uuid.uuid4())
            if changed == "selection": task["selection"]["model"] = "other"
            if changed == "command": task["events"][0]["input"] = json.dumps({"command": "other"})
            if changed == "applied": task["changes"][0]["applied"] = True
            with self.subTest(changed=changed), self.assertRaises(qa.qa.QaFailure):
                qa.validate(task, PROJECT, VM, selection, qa.command(RUN, "cancel", 90), RUN, "cancel", "cancelled", NONCE)

    def test_disconnected_admission_sends_once_and_does_not_read_or_resubmit(self):
        with patch.object(qa.socket, "create_connection") as connect:
            client = connect.return_value.__enter__.return_value
            qa.disconnected_put(Api(), "/api/owned-task", {"kind": "code"})
            client.sendall.assert_called_once()
            self.assertIn(b"PUT /api/owned-task HTTP/1.1\r\n", client.sendall.call_args.args[0])
            client.recv.assert_not_called()

    def test_provider_failure_retains_owned_source_and_report_without_cleanup_or_faults(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = SimpleNamespace(run_id=RUN, connection=CONNECTION, vm=VM, model="verified",
                                   owner_marker=Path(temporary) / "owner", vms_dir=Path(temporary),
                                   results_dir=Path(temporary) / "evidence", controller_unit="firecrab-api.service")
            api = Api()
            with patch.object(qa, "owner_marker"), patch.object(qa, "controller_pid", return_value="42"), \
                 patch.object(qa, "binary_digest", return_value="e" * 64), \
                 patch.object(qa, "Guest") as guest, patch.object(qa, "control", return_value="root") as control, \
                 patch.object(qa.os, "chown"):
                guest.return_value.run.return_value = (0, NONCE)
                with self.assertRaises(qa.qa.QaFailure), contextlib.redirect_stdout(io.StringIO()):
                    qa.run_gate(args, api)
                self.assertTrue((args.results_dir / "source/executions.txt").exists())
                report = json.loads((args.results_dir / "summary.json").read_text())
                self.assertEqual(report["status"], "FAILED")
                self.assertTrue(report["retainedOnFailure"])
                self.assertEqual(report["vmId"], VM)
                self.assertEqual(stat.S_IMODE((args.results_dir / "summary.json").stat().st_mode), 0o600)
                self.assertFalse(any(method == "DELETE" or "/stop" in path for method, path, _ in api.calls))
                self.assertTrue(all(call.args[0] == "show" for call in control.call_args_list))


if __name__ == "__main__":
    unittest.main()
