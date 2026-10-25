#!/usr/bin/env python3
"""Exercise persistent volumes on a dedicated Linux/KVM API host.

Run inside the API's network namespace. The API unit must support stop/start.
Only resources created by this run are removed. Fault injection touches only
those volumes and requires their storage root and database to be local.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs).stdout


class QA:
    def __init__(self, args):
        self.args = args
        self.created = []
        self.checks = []
        self.network = None
        self.volume = None
        self.keys = tempfile.TemporaryDirectory(prefix="f247-keys-")
        self.output = Path(args.output)
        self.output.mkdir(parents=True, exist_ok=True)
        self.prefix = "qa247-" + uuid.uuid4().hex[:8]

    def api(self, path, body=None, method=None, status=200, raw=False):
        request = urllib.request.Request(self.args.api.rstrip("/") + path,
            None if body is None else json.dumps(body).encode(),
            {"Content-Type": "application/json"}, method=method)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                code, data = response.status, response.read()
        except urllib.error.HTTPError as error:
            code, data = error.code, error.read()
        assert code == status, (path, code, data.decode(errors="replace"))
        return data if raw else json.loads(data) if data else None

    def passed(self, name, **evidence):
        self.checks.append({"check": name, "result": "PASS", **evidence})
        print("PASS", name, json.dumps(evidence), flush=True)

    def ready(self):
        for _ in range(60):
            try:
                self.api("/api/microblockstorage")
                return
            except (OSError, AssertionError):
                time.sleep(1)
        raise AssertionError("API did not recover")

    def vm(self, name, attached=False):
        mounts = [{"volumeId": self.volume["id"], "mountPath": "/data"}] if attached else []
        vm = self.api("/api/vms", {"name": self.prefix + name, "template": self.args.template,
            "cpu": 1, "ram": 512, "diskGb": self.args.disk_gib,
            "microNetworkId": self.network["id"], "blockStorages": mounts}, status=201)
        self.created.append(vm["id"])
        return vm

    def wait(self, vm, state):
        for _ in range(120):
            current = self.api("/api/vms/" + vm["id"])
            if current["state"] == state:
                return current
            if state == "running" and current["state"] == "error":
                raise AssertionError(current)
            time.sleep(1)
        raise AssertionError(("VM state timeout", state, current))

    def start(self, vm):
        self.api("/api/vms/" + vm["id"] + "/start", {})
        return self.wait(vm, "running")

    def stop(self, vm):
        current = self.api("/api/vms/" + vm["id"])
        if current["state"] in ("created", "stopped", "error"):
            return current
        self.api("/api/vms/" + vm["id"] + "/stop", {})
        return self.wait(vm, "stopped")

    def ssh(self, vm, command, data=None):
        folder = Path(self.keys.name)
        key, known = folder / vm["id"], folder / (vm["id"] + ".known")
        if not key.exists():
            key.write_bytes(self.api("/api/vms/" + vm["id"] + "/ssh-key", raw=True))
            key.chmod(0o600)
            public = self.api("/api/vms/" + vm["id"] + "/ssh-host-key")["publicKey"]
            known.write_text(vm["ipv4"] + " " + public.strip() + "\n")
        args = ["ssh", "-i", str(key), "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "ConnectTimeout=3", "-o", "StrictHostKeyChecking=yes",
            "-o", "UserKnownHostsFile=" + str(known), "root@" + vm["ipv4"], command]
        for _ in range(30):
            result = subprocess.run(args, input=data, capture_output=True)
            if result.returncode == 0:
                return result.stdout.decode().strip()
            time.sleep(1)
        raise AssertionError((command, result.returncode, result.stderr.decode()))

    def disk(self):
        return Path(self.args.storage_root) / "volumes" / self.volume["id"] / "data.ext4"

    def rootfs(self, vm):
        return next((Path(self.args.storage_root) / "vms" / vm["id"].replace("-", "") / "d").glob("*.ext4"))

    def edit(self, disk, command):
        return run("debugfs", "-w", "-R", command, str(disk))

    def digest(self, vm):
        return self.ssh(vm, "mountpoint -q /data && sha256sum /data/proof").split()[0]

    def restart(self, crash=False):
        if crash:
            run("systemctl", "kill", "--kill-whom=main", "--signal=KILL", self.args.api_unit)
        else:
            run("systemctl", "restart", self.args.api_unit)
        time.sleep(2)
        self.ready()

    def exercise(self):
        self.ready()
        assert not self.api("/api/vms"), "Use a dedicated API with no existing VMs"
        self.network = self.api("/api/micro-networks", {"name": self.prefix, "subnetCidr": "172.30.247.0/24", "internetEnabled": False}, status=201)
        self.volume = json.loads(run(self.args.cli, "--api", self.args.api, "volume", "create", self.prefix, "--size", "1", "--json"))
        a = self.vm("-a", True)
        b = self.vm("-b")
        self.start(a)
        payload = (b"Firecrab #247 real KVM persistent volume\n" * 2048)
        digest = hashlib.sha256(payload).hexdigest()
        self.ssh(a, "cat > /data/proof && sync", payload)
        assert self.digest(a) == digest
        self.passed("guest-mount-and-write", sha256=digest)
        volume_path = "/api/microblockstorage/" + self.volume["id"]
        self.api(volume_path, method="DELETE", status=409)
        self.api(volume_path + "/detach", {}, status=409)
        self.api(volume_path + "/attach", {"vmId": b["id"], "mountPath": "/data"}, status=409)
        self.api("/api/vms/" + a["id"] + "/pause", {})
        self.wait(a, "paused")
        self.api(volume_path + "/detach", {}, status=409)
        self.api("/api/vms/" + a["id"] + "/resume", {})
        self.wait(a, "running")
        self.passed("exclusive-owner-running-paused-rejections")
        unit = "firecrab-vm-" + a["id"].replace("-", "")
        pid = run("systemctl", "show", "-p", "MainPID", "--value", unit).strip()
        for crash in (False, True):
            self.restart(crash)
            assert run("systemctl", "show", "-p", "MainPID", "--value", unit).strip() == pid
            assert self.api(volume_path)["attachedVmId"] == a["id"]
            assert self.digest(a) == digest
            self.passed("api-crash-recovery" if crash else "api-restart-live-adoption", shim_pid=pid.decode())
        self.stop(a)
        self.start(a)
        assert self.digest(a) == digest
        self.passed("stop-start-retains-mount-and-data")
        self.stop(a)
        self.api("/api/vms/" + a["id"], method="DELETE", status=204)
        self.created.remove(a["id"])
        assert self.disk().exists() and self.api(volume_path)["state"] == "available"
        run(self.args.cli, "--api", self.args.api, "volume", "attach", self.prefix, "--vm", b["id"], "--mount-path", "/data", "--json")
        self.start(b)
        assert self.digest(b) == digest
        self.passed("delete-a-reattach-b-checksum", sha256=digest)
        self.stop(b)
        # Install an observable application into this VM's private rootfs only.
        app = self.output / "qa-app.sh"
        app.write_text("#!/bin/sh\nprintf started > /qa247-app-started\nexec sleep 3600\n")
        self.edit(self.rootfs(b), "rm /etc/firecrab/services.d/app")
        self.edit(self.rootfs(b), f"write {app} /etc/firecrab/services.d/app")
        self.edit(self.rootfs(b), "set_inode_field /etc/firecrab/services.d/app mode 0100755")
        self.start(b)
        assert self.ssh(b, "cat /qa247-app-started") == "started"
        self.stop(b)
        self.edit(self.rootfs(b), "rm /qa247-app-started")
        disk = self.disk()
        missing = disk.with_suffix(".missing")
        disk.rename(missing)
        try:
            self.api("/api/vms/" + b["id"] + "/start", {})
            failed = self.wait(b, "error")
            assert any("volume" in (x.get("detail") or "").lower() for x in failed["startupTimeline"])
            assert not disk.exists()
            self.passed("missing-disk-fails-without-replacement")
        finally:
            missing.rename(disk)
        self.stop(b)
        backup = disk.with_suffix(".backup")
        run("cp", "--reflink=auto", "--sparse=always", str(disk), str(backup))
        try:
            # Unknown ext4 incompatibility bits preserve UUID/signature but fail the guest mount.
            with disk.open("r+b") as stream:
                stream.seek(1024 + 96)
                stream.write(struct.pack("<I", 0xffffffff))
                stream.flush()
                os.fsync(stream.fileno())
            self.api("/api/vms/" + b["id"] + "/start", {})
            failed = self.wait(b, "error")
            self.stop(b)
            consoles = list((disk.parents[2] / "vms" / b["id"].replace("-", "") / "r").glob("*/console.log"))
            console = max(consoles, key=lambda p: p.stat().st_mtime).read_text(errors="replace")
            assert "volume-mount-failed" in console and "mount-failed" in console
            marker = subprocess.run(["debugfs", "-R", "stat /qa247-app-started", str(self.rootfs(b))], capture_output=True)
            assert b"File not found" in marker.stderr
            (self.output / "mount-failure-console.log").write_text(console)
            self.passed("guest-mount-failure-blocks-application")
        finally:
            os.replace(backup, disk)
        self.start(b)
        assert self.digest(b) == digest
        self.passed("repaired-disk-restores-original-data")
        self.ssh(b, "printf synced-before-crash > /data/crash-proof && sync")
        unit = "firecrab-vm-" + b["id"].replace("-", "")
        shim = run("systemctl", "show", "-p", "MainPID", "--value", unit).decode().strip()
        children = run("pgrep", "-P", shim).decode().split()
        vmm = [pid for pid in children if Path("/proc", pid, "comm").read_text().strip() == "firecracker"]
        assert len(vmm) == 1
        run("kill", "-KILL", vmm[0])
        self.wait(b, "error")
        assert self.api(volume_path)["attachedVmId"] == b["id"]
        self.start(b)
        assert self.digest(b) == digest
        assert self.ssh(b, "cat /data/crash-proof") == "synced-before-crash"
        self.passed("vmm-crash-recovers-synced-data")
        run("systemctl", "stop", unit)
        self.wait(b, "stopped")
        self.passed("external-unit-stop-retains-attachment")
        self.api(volume_path + "/detach", {})
        c = self.vm("-c")
        def attach(vm):
            try:
                self.api(volume_path + "/attach", {"vmId": vm["id"], "mountPath": "/data"})
                return 200
            except AssertionError as error:
                assert error.args[0][1] == 409
                return 409
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            assert sorted(pool.map(attach, (b, c))) == [200, 409]
        self.passed("concurrent-attachment-one-winner")
        owner = self.api("/api/vms/" + self.api(volume_path)["attachedVmId"])
        self.api("/api/vms/" + owner["id"] + "/start", {})
        owner_unit = "firecrab-vm-" + owner["id"].replace("-", "")
        for _ in range(100):
            probe = subprocess.run(["systemctl", "show", "-p", "MainPID", "--value", owner_unit], capture_output=True)
            if probe.stdout.strip() not in (b"", b"0"):
                break
            time.sleep(0.1)
        else:
            raise AssertionError("interrupted-start fixture never launched a shim")
        assert self.api("/api/vms/" + owner["id"])["state"] == "starting"
        self.restart(crash=True)
        interrupted = self.api("/api/vms/" + owner["id"])
        assert interrupted["state"] == "error", interrupted
        assert self.api(volume_path)["attachedVmId"] == owner["id"]
        self.start(owner)
        assert self.digest(owner) == digest
        self.stop(owner)
        self.passed("interrupted-start-retains-exclusive-reservation")
        self.api(volume_path + "/detach", {})
        run("systemctl", "stop", self.args.api_unit)
        try:
            conn = sqlite3.connect(self.args.database)
            pending = []
            for phase in ("creating", "creating", "deleting"):
                identity = str(uuid.uuid4())
                path = Path(self.args.storage_root) / "volumes" / identity / "data.ext4"
                path.parent.mkdir(parents=True)
                pending.append((identity, path, phase))
                conn.execute("INSERT INTO micro_block_storages(id,name,size_gib,storage_root,phase) VALUES(?,?,1,'default',?)", (identity, self.prefix + identity, phase))
            conn.commit()
            conn.close()
            for i, (_, path, _) in enumerate(pending):
                with path.open("wb") as stream:
                    stream.truncate(1 << 30)
                if i != 0:
                    run("mkfs.ext4", "-q", "-F", "-U", pending[i][0], "-E", "lazy_itable_init=0,lazy_journal_init=0", str(path))
        finally:
            run("systemctl", "start", self.args.api_unit)
            self.ready()
        rows = {v["id"]: v for v in self.api("/api/microblockstorage")}
        assert pending[0][0] not in rows and not pending[0][1].exists()
        assert rows[pending[1][0]]["state"] == "available"
        assert pending[2][0] not in rows and not pending[2][1].exists()
        self.api("/api/microblockstorage/" + pending[1][0], method="DELETE", status=204)
        self.passed("interrupted-create-delete-recovery")

    def cleanup(self):
        self.ready()
        for identity in self.created:
            try:
                vm = self.api("/api/vms/" + identity)
                if vm["state"] not in ("created", "stopped", "error"):
                    self.stop(vm)
                self.api("/api/vms/" + identity, method="DELETE", status=204)
            except Exception as error:
                print("cleanup VM:", error, flush=True)
        if self.volume:
            run(self.args.cli, "--api", self.args.api, "volume", "delete", self.prefix)
            assert not self.disk().exists()
        if self.network:
            self.api("/api/micro-networks/" + self.network["id"], method="DELETE", status=204)
        self.keys.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True)
    parser.add_argument("--api-unit", required=True)
    parser.add_argument("--template", required=True)
    parser.add_argument("--storage-root", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--disk-gib", type=int, default=8)
    args = parser.parse_args()
    qa = QA(args)
    success = False
    try:
        qa.exercise()
        success = True
    finally:
        try:
            qa.cleanup()
        except Exception:
            success = False
            raise
        finally:
            (qa.output / "results.json").write_text(json.dumps({"result": "PASS" if success else "FAILED", "kernel": os.uname().release,
                "checks": qa.checks}, indent=2) + "\n")


if __name__ == "__main__":
    main()
