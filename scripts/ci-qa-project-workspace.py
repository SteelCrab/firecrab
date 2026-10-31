#!/usr/bin/env python3
"""Real project-workspace recovery in an owned Linux/KVM network namespace.

Prepare root/bin/{firecrab-api,firecrab-helper} and root/images first. The
operator creates root/owner containing the run UUID. This harness never stops
installed API/helper services. Retain the private root and report for inspection.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.request
import urllib.parse
import uuid


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT).strip()


def wait(probe, seconds=180):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = probe()
        if result is not None:
            return result
        time.sleep(0.5)
    raise RuntimeError("Timed out waiting for the owned QA resource")


def validate_owner(root, run_id):
    if not root.is_absolute() or root.resolve() != root:
        raise ValueError("Use an absolute, canonical QA root")
    for path in (root, root / "owner"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
            raise ValueError("QA root and marker must be private and owned by the operator")
    if not (root / "owner").is_file() or (root / "owner").read_text() != str(run_id):
        raise ValueError("QA ownership marker does not match the run UUID")
    for name in ("firecrab-api", "firecrab-helper"):
        binary = root / "bin" / name
        info = binary.lstat()
        if binary.resolve() != binary or not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise ValueError("QA binaries must be regular operator-owned files without shared writes")


def stop_owned(process, binary, directory, crash=False):
    if process.poll() is not None:
        return
    proc = Path("/proc") / str(process.pid)
    if (proc / "exe").resolve() != binary.resolve() or (proc / "cwd").resolve() != directory.resolve():
        raise RuntimeError("Refusing to signal a process outside the owned QA instance")
    process.send_signal(signal.SIGKILL if crash else signal.SIGTERM)
    process.wait(timeout=20)


class QA:
    def __init__(self, args, storage):
        self.args, self.root, self.storage = args, args.root, storage
        self.base = "http://127.0.0.1:19550"
        self.project = str(uuid.uuid4())
        self.workspace = f"/api/ai-sandbox/projects/{self.project}/workspace"
        self.vms, self.dropins = [], []
        self.api_process = None
        self.file_save = None
        self.env = {
            "PATH": str(self.root / "bin") + ":/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "FIRECRAB_BIND_ADDR": "127.0.0.1:19550",
            "FIRECRAB_STORAGE_ROOTS": "qa=" + str(storage),
            "FIRECRAB_IMAGE_ROOT": str(self.root / "images"),
            "FIRECRAB_NET_HELPER_SOCK": str(self.root / "helper.sock"),
            "FIRECRAB_LIBDIR": str(self.root / "bin"),
        }
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self.report = {
            "runId": str(args.run_id), "startedAt": datetime.now(timezone.utc).isoformat(),
            "apiSha256": hashlib.sha256((self.root / "bin/firecrab-api").read_bytes()).hexdigest(),
            "helperSha256": hashlib.sha256((self.root / "bin/firecrab-helper").read_bytes()).hexdigest(),
            "harnessSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "architecture": run("uname", "-m"), "firecracker": run("firecracker", "--version"),
            "projectId": self.project, "checks": [], "status": "RUNNING",
            "limits": ["No editor or agent integration", "Synced writes; no unflushed power-loss claim"],
        }

    def save(self):
        target = self.root / "evidence/summary.json"
        temporary = target.with_suffix(".partial")
        temporary.write_text(json.dumps(self.report, indent=2) + "\n")
        temporary.chmod(0o600)
        os.replace(temporary, target)

    def passed(self, name, **evidence):
        self.report["checks"].append({"check": name, **evidence})
        self.save()
        print("PASS", name, json.dumps(evidence), flush=True)

    def request(self, method, path, body=None, status=None, raw=False):
        payload = json.dumps(body).encode() if body is not None else (b"" if method in ("PUT", "POST") else None)
        request = urllib.request.Request(self.base + path, data=payload, method=method,
            headers={"Content-Type": "application/json", "Origin": self.base})
        try:
            response = self.opener.open(request, timeout=90)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read(2 * 1024 * 1024)
            if (status is not None and response.code != status) or (status is None and not 200 <= response.code < 300):
                raise AssertionError((method, path, response.code, data.decode(errors="replace")))
        return data if raw else json.loads(data) if data else None

    def start_api(self):
        log = (self.root / "api.log").open("ab")
        self.api_process = subprocess.Popen([str(self.root / "bin/firecrab-api")],
            cwd=self.root / "api", env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        log.close()
        def ready():
            if self.api_process.poll() is not None:
                raise RuntimeError((self.root / "api.log").read_text()[-4000:])
            try:
                self.request("GET", "/api/vms")
                return True
            except (OSError, urllib.error.URLError):
                return None
        wait(ready, 30)

    def metadata(self):
        data = self.request("GET", self.workspace)
        assert data["databaseId"] == self.database
        assert data["volumeId"] == self.volume["id"]
        assert data["revision"] == 1 and data["document"] == self.document
        return data

    def create_vm(self):
        vm = self.request("POST", "/api/vms", {"name": "qa550-" + str(uuid.uuid4())[:8],
            "template": self.args.template, "cpu": 1, "ram": 512, "diskGb": self.disk_gib,
            "storageRoot": "qa", "microNetworkId": self.network["id"]})
        self.vms.append(vm["id"])
        (self.root / "owned-vms.json").write_text(json.dumps(self.vms))
        dropin = Path("/run/systemd/system") / ("firecrab-vm-" + uuid.UUID(vm["id"]).hex + ".service.d")
        dropin.mkdir()
        (dropin / "qa550.conf").write_text("[Service]\nNetworkNamespacePath=/run/netns/" + self.args.namespace + "\n")
        self.dropins.append(dropin)
        run("systemctl", "daemon-reload")
        return vm

    def vm_state(self, vm, expected):
        current = self.request("GET", "/api/vms/" + vm["id"])
        if expected == "running":
            unit = "firecrab-vm-" + uuid.UUID(vm["id"]).hex + ".service"
            pid = run("systemctl", "show", unit, "-p", "MainPID", "--value")
            if pid.isdigit() and int(pid):
                expected_inode = Path("/run/netns", self.args.namespace).stat().st_ino
                actual_inode = Path("/proc", pid, "ns/net").stat().st_ino
                assert actual_inode == expected_inode, "VM unit escaped the owned QA network namespace"
                self.report.setdefault("runtimeNamespaces", {})[pid] = {
                    "vmId": vm["id"], "namespaceInode": actual_inode}
        if expected == "running" and current["state"] == "error":
            raise RuntimeError(self.request("GET", "/api/vms/" + vm["id"] + "/log"))
        return current if current["state"] == expected else None

    def start_vm(self, vm):
        self.request("POST", "/api/vms/" + vm["id"] + "/start")
        vm = wait(lambda: self.vm_state(vm, "running"), 240)
        self.ssh_args = ["ssh", "-F", "/dev/null", "-T", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=3"]
        key, hosts = self.root / (vm["id"] + ".key"), self.root / (vm["id"] + ".known_hosts")
        key.write_bytes(self.request("GET", "/api/vms/" + vm["id"] + "/ssh-key", raw=True))
        key.chmod(0o600)
        public = self.request("GET", "/api/vms/" + vm["id"] + "/ssh-host-key")["publicKey"]
        hosts.write_text(vm["ipv4"] + " " + public.strip() + "\n")
        self.ssh_args += ["-i", str(key), "-o", "UserKnownHostsFile=" + str(hosts), "root@" + vm["ipv4"]]
        def probe():
            result = subprocess.run(self.ssh_args + ["cat /proc/sys/kernel/random/boot_id"],
                capture_output=True, text=True, timeout=10)
            return result.stdout.strip() if result.returncode == 0 else None
        return vm, wait(probe, 180)

    def ssh(self, command):
        return subprocess.check_output(self.ssh_args + [command], text=True, stderr=subprocess.STDOUT, timeout=30).strip()

    def digest(self):
        return self.ssh("sha256sum /workspace/shared/qa550-proof").split()[0]

    def stop_vm(self, vm):
        self.request("POST", "/api/vms/" + vm["id"] + "/stop")
        return wait(lambda: self.vm_state(vm, "stopped"), 90)

    def file_url(self, suffix=""):
        return self.workspace + "/files" + suffix + "?path=" + urllib.parse.quote("src/qa550-live.txt", safe="")

    def file_contents(self):
        item = self.request("GET", self.file_url())
        assert base64.b64decode(item["contentBase64"], validate=True) == self.file_bytes
        assert item["version"]["id"] == self.file_save["version"]["id"]
        return item

    def file_history(self):
        history = self.request("GET", self.file_url("/history"))
        assert all(v["path"] == "src/qa550-live.txt" for v in history)
        assert self.file_save["version"]["id"] in [v["id"] for v in history]
        global_history = self.request("GET", self.workspace + "/files/history")
        assert any(v["path"] == "src/qa550-live.txt" for v in global_history)
        saved = self.request("GET", self.file_url("/versions/" + str(self.file_save["version"]["id"])))
        assert base64.b64decode(saved["contentBase64"], validate=True) == self.file_bytes
        return history

    def exercise_files(self):
        first = ("first:" + str(uuid.uuid4()) + "\n").encode()
        initial = {"operationId": str(uuid.uuid4()), "expectedVersion": 0,
            "contentBase64": base64.b64encode(first).decode()}
        created = self.request("PUT", self.file_url(), initial)
        assert base64.b64decode(created["contentBase64"], validate=True) == first
        tree = self.request("GET", self.workspace + "/files/tree?path=src")
        assert any(e["name"] == "qa550-live.txt" and not e["directory"] for e in tree)
        stale = dict(initial, operationId=str(uuid.uuid4()), contentBase64=base64.b64encode(b"stale").decode())
        self.request("PUT", self.file_url(), stale, status=409)
        second = b"second:" + os.urandom(64)
        changed = self.request("PUT", self.file_url(), {"operationId": str(uuid.uuid4()),
            "expectedVersion": created["version"]["id"], "contentBase64": base64.b64encode(second).decode()})
        difference = self.request("GET", self.file_url("/diff") + "&from=" + str(created["version"]["id"]) + "&to=" + str(changed["version"]["id"]))
        assert base64.b64decode(difference["before"]["contentBase64"]) == first
        assert base64.b64decode(difference["after"]["contentBase64"]) == second
        restored = self.request("POST", self.file_url("/restore"), {"operationId": str(uuid.uuid4()),
            "expectedVersion": changed["version"]["id"], "version": created["version"]["id"]})
        assert restored["version"]["restoredFrom"] == created["version"]["id"]
        assert restored["version"]["id"] > changed["version"]["id"]
        self.file_bytes = b"retained:" + os.urandom(64)
        self.file_request = {"operationId": str(uuid.uuid4()), "expectedVersion": restored["version"]["id"],
            "contentBase64": base64.b64encode(self.file_bytes).decode()}
        self.file_save = self.request("PUT", self.file_url(), self.file_request)
        assert self.request("PUT", self.file_url(), initial)["version"]["id"] == created["version"]["id"]
        self.file_contents()
        assert len(self.file_history()) == 4
        self.passed("file-api-conflict-diff-immutable-history-and-restore",
            version=self.file_save["version"]["id"], fileSha256=hashlib.sha256(self.file_bytes).hexdigest())
        self.request("GET", self.workspace + "/files?path=.firecrab-workspace/ready", status=400)
        self.ssh("ln -s /etc/passwd /workspace/shared/qa550-forbidden-link")
        self.request("GET", self.workspace + "/files?path=qa550-forbidden-link", status=400)
        self.passed("file-api-rejects-private-metadata-and-symlink")
        disconnected = {"operationId": str(uuid.uuid4()), "expectedVersion": self.file_save["version"]["id"],
            "contentBase64": base64.b64encode(b"after-disconnect:" + os.urandom(64)).decode()}
        client = http.client.HTTPConnection("127.0.0.1", 19550, timeout=10)
        client.request("PUT", self.file_url(), json.dumps(disconnected),
            headers={"Content-Type": "application/json", "Origin": self.base})
        client.close()
        time.sleep(0.5)
        self.file_save = self.request("PUT", self.file_url(), disconnected)
        self.file_request = disconnected
        self.file_bytes = base64.b64decode(disconnected["contentBase64"])
        self.file_contents()
        assert len(self.file_history()) == 5
        operations = self.request("GET", self.workspace + "/file-operations")
        assert len([op for op in operations if op["id"] == disconnected["operationId"]]) == 1
        self.passed("disconnected-save-retry-keeps-one-operation-and-version")

    def exercise(self):
        self.start_api()
        assert not self.request("GET", "/api/vms"), "QA database already contains VMs"
        assert not self.request("GET", "/api/ai-sandbox/projects"), "QA database already contains projects"
        images = self.request("GET", "/api/images")
        image = next(image for image in images if image["alias"] == self.args.template and image["installed"])
        self.disk_gib = image["minDiskGb"]
        self.report["template"] = image
        self.network = self.request("POST", "/api/micro-networks", {"name": "qa550", "subnetCidr": "172.23.55.0/24",
            "internetEnabled": False, "uplink": "qa-uplink"})
        self.volume = self.request("POST", "/api/microblockstorage", {"name": "qa550", "sizeGib": 1, "storageRoot": "qa"})
        def volume_ready():
            data = self.request("GET", "/api/microblockstorage/" + self.volume["id"])
            return data if data["state"] == "available" else None
        self.volume = wait(volume_ready)
        vm = self.create_vm()
        spec = {"name": "qa550", "vmId": vm["id"]}
        self.request("PUT", "/api/ai-sandbox/projects/" + self.project, spec)
        bound = self.request("PUT", self.workspace, {"volumeId": self.volume["id"]})
        self.database = bound["databaseId"]
        self.document = {"run": str(self.args.run_id), "retained": str(uuid.uuid4())}
        self.request("PUT", self.workspace + "/state", {"expectedRevision": 0, "document": self.document})
        vm, boot = self.start_vm(vm)
        ready = self.request("POST", self.workspace + "/reconcile")
        assert ready["status"] == "ready" and ready["initialized"]
        content = (str(self.args.run_id) + ":" + str(uuid.uuid4()) + "\n").encode()
        expected_digest = hashlib.sha256(content).hexdigest()
        encoded = base64.b64encode(content).decode()
        self.ssh("set -eu; printf '%s' '" + encoded + "' | base64 -d > /workspace/shared/qa550-proof; sync")
        assert self.digest() == expected_digest
        self.metadata()
        self.passed("real-ext4-mount-and-synced-write", vmId=vm["id"], bootId=boot,
            volumeId=self.volume["id"], databaseId=self.database, fileSha256=expected_digest)
        if self.args.files:
            self.exercise_files()
        client = http.client.HTTPConnection("127.0.0.1", 19550, timeout=10)
        client.request("GET", self.workspace)
        assert client.getresponse().status == 200
        client.close()
        self.metadata()
        self.passed("client-disconnect-and-new-client-retain-metadata")
        unit = "firecrab-vm-" + uuid.UUID(vm["id"]).hex + ".service"
        shim = run("systemctl", "show", unit, "-p", "MainPID", "--value")
        for crash in (False, True):
            old_pid = self.api_process.pid
            stop_owned(self.api_process, self.root / "bin/firecrab-api", self.root / "api", crash)
            self.start_api()
            assert self.api_process.pid != old_pid
            assert run("systemctl", "show", unit, "-p", "MainPID", "--value") == shim
            assert self.ssh("cat /proc/sys/kernel/random/boot_id") == boot
            assert self.digest() == expected_digest
            assert self.metadata()["vmId"] == vm["id"]
            if self.args.files:
                self.file_contents()
                self.file_history()
                assert self.request("PUT", self.file_url(), self.file_request)["version"]["id"] == self.file_save["version"]["id"]
            self.passed("api-crash-recovery" if crash else "api-restart-recovery",
                oldApiPid=old_pid, newApiPid=self.api_process.pid, shimPid=shim)
        self.stop_vm(vm)
        assert self.metadata()["status"] == "stopped"
        if self.args.files:
            self.file_history()
            self.request("GET", self.file_url(), status=409)
            assert self.request("PUT", self.file_url(), self.file_request)["version"]["id"] == self.file_save["version"]["id"]
            self.passed("file-history-and-completed-save-queryable-with-stopped-vm")
        self.passed("project-database-queryable-with-stopped-vm")
        vm, new_boot = self.start_vm(vm)
        assert new_boot != boot
        assert self.metadata()["status"] == "awaiting_mount"
        assert self.request("POST", self.workspace + "/reconcile")["status"] == "ready"
        assert self.digest() == expected_digest
        if self.args.files:
            self.file_contents()
            self.file_history()
        self.passed("real-vm-stop-start-retains-data-and-requires-new-proof", oldBootId=boot, newBootId=new_boot)
        self.stop_vm(vm)
        self.request("DELETE", "/api/vms/" + vm["id"])
        assert self.metadata()["status"] == "unavailable"
        assert self.request("GET", "/api/microblockstorage/" + self.volume["id"])["state"] == "available"
        self.passed("vm-deletion-preserves-project-database-and-volume")
        replacement = self.create_vm()
        spec["vmId"] = replacement["id"]
        self.request("PUT", "/api/ai-sandbox/projects/" + self.project, spec)
        self.request("PUT", self.workspace, {"volumeId": self.volume["id"], "expectedGeneration": 0}, status=409)
        assert self.metadata()["generation"] == 1
        self.passed("stale-generation-cannot-rebind-retained-data")
        bound = self.request("PUT", self.workspace, {"volumeId": self.volume["id"], "expectedGeneration": 1})
        assert bound["databaseId"] == self.database and bound["generation"] == 2
        replacement, replacement_boot = self.start_vm(replacement)
        assert replacement_boot != new_boot and replacement["id"] != vm["id"]
        assert self.request("POST", self.workspace + "/reconcile")["status"] == "ready"
        assert self.digest() == expected_digest and self.metadata()["generation"] == 2
        if self.args.files:
            self.file_contents()
            self.file_history()
            self.passed("file-content-and-history-survive-runtime-replacement")
        self.passed("new-runtime-rebinds-same-retained-database-and-file", oldVmId=vm["id"],
            newVmId=replacement["id"], newBootId=replacement_boot, fileSha256=expected_digest)
        self.stop_vm(replacement)
        retry = self.request("PUT", self.workspace, {"volumeId": self.volume["id"], "expectedGeneration": 1})
        assert retry["generation"] == 2 and retry["databaseId"] == self.database
        self.passed("identical-binding-retry-is-idempotent")
        disk = self.storage / "volumes" / self.volume["id"] / "data.ext4"
        retained = disk.with_suffix(".retained")
        assert not retained.exists()
        disk.rename(retained)
        try:
            assert self.metadata()["status"] == "unavailable" and not disk.exists()
        finally:
            retained.rename(disk)
        self.passed("missing-volume-is-unavailable-without-replacement")
        self.request("DELETE", "/api/ai-sandbox/projects/" + self.project, status=409)
        self.request("DELETE", "/api/microblockstorage/" + self.volume["id"], status=409)
        self.request("POST", "/api/microblockstorage/" + self.volume["id"] + "/detach", status=409)
        assert self.metadata()["status"] == "stopped"
        self.report["status"] = "PASS"
        self.passed("ordinary-delete-and-detach-preserve-retained-project-data")


def inside(args):
    assert run("ip", "netns", "identify", str(os.getpid())) == args.namespace
    links = json.loads(run("ip", "-j", "link"))
    assert {link["ifname"] for link in links} == {"lo", "qa-uplink"}, "requires a fresh private network namespace"
    qa = QA(args, args.storage)
    log = (args.root / "helper.log").open("ab")
    helper = subprocess.Popen(["unshare", "--mount", "--propagation", "private", "sh", "-c",
        'set -eu; mount --bind "$1/run" /run/firecrab; mount --bind "$1/hosts" /etc/hosts; exec "$1/bin/firecrab-helper"',
        "qa550", str(args.root)], env=qa.env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
    log.close()
    try:
        wait(lambda: True if (args.root / "helper.sock").exists() else None, 15)
        qa.exercise()
    except Exception as error:
        qa.report.update(status="FAIL", error=str(error))
        raise
    finally:
        for vm in qa.vms:
            unit = "firecrab-vm-" + uuid.UUID(vm).hex + ".service"
            subprocess.run(["systemctl", "stop", unit], capture_output=True, timeout=60)
            subprocess.run(["systemctl", "reset-failed", unit], capture_output=True)
        if qa.api_process is not None:
            stop_owned(qa.api_process, args.root / "bin/firecrab-api", args.root / "api")
        helper.terminate()
        helper.wait(timeout=15)
        qa.report["finishedAt"] = datetime.now(timezone.utc).isoformat()
        qa.save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--run-id", type=uuid.UUID, required=True)
    parser.add_argument("--template", default="ubuntu-26.04")
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--files", action="store_true", help="also exercise the native file API and retained history")
    parser.add_argument("--namespace", help=argparse.SUPPRESS)
    parser.add_argument("--storage", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.live or sys.platform != "linux" or os.geteuid() != 0 or not Path("/dev/kvm").is_char_device():
        parser.error("requires --live and root on a Linux/KVM host")
    validate_owner(args.root, args.run_id)
    if (args.root / "prepare-api.pid").exists():
        parser.error("stop the owned image-preparation API before live acceptance")
    os.umask(0o077)
    if args.namespace:
        inside(args)
        return
    name = "fc550-" + args.run_id.hex[:10]
    storage = Path("/q550" + args.run_id.hex[:8])
    storage.mkdir(mode=0o700)
    before = run("systemctl", "list-units", "--type=service", "--no-legend", "firecrab-vm-*")
    primary = run("systemctl", "show", "firecrab-api", "-p", "MainPID", "--value")
    mounted = created = False
    try:
        run("mount", "--bind", str(args.root / "store"), str(storage))
        mounted = True
        run("ip", "netns", "add", name)
        created = True
        run("ip", "-n", name, "link", "set", "lo", "up")
        run("ip", "-n", name, "link", "add", "qa-uplink", "type", "dummy")
        run("ip", "-n", name, "addr", "add", "192.0.2.1/24", "dev", "qa-uplink")
        run("ip", "-n", name, "link", "set", "qa-uplink", "up")
        run("ip", "-n", name, "route", "add", "default", "dev", "qa-uplink")
        (args.root / "run").mkdir(mode=0o700)
        # dnsmasq drops privileges before reloading reservations through this
        # private mount. Only its non-secret runtime directory is traversable.
        (args.root / "run").chmod(0o755)
        (args.root / "hosts").write_text(Path("/etc/hosts").read_text() + "\n192.0.2.42 example.com\n")
        (args.root / "hosts").chmod(0o644)
        subprocess.run(["ip", "netns", "exec", name, "python3", str(Path(__file__).resolve()),
            "--root", str(args.root), "--run-id", str(args.run_id), "--template", args.template,
            "--namespace", name, "--storage", str(storage), "--live"] + (["--files"] if args.files else []), check=True)
    finally:
        owned = args.root / "owned-vms.json"
        if owned.exists():
            for vm in json.loads(owned.read_text()):
                dropin = Path("/run/systemd/system") / ("firecrab-vm-" + uuid.UUID(vm).hex + ".service.d")
                config = dropin / "qa550.conf"
                if config.exists() and config.read_text() == "[Service]\nNetworkNamespacePath=/run/netns/" + name + "\n":
                    config.unlink()
                    dropin.rmdir()
            run("systemctl", "daemon-reload")
        if created:
            for pid in run("ip", "netns", "pids", name).split():
                try:
                    os.kill(int(pid), signal.SIGTERM)
                except ProcessLookupError:
                    pass
            run("ip", "netns", "delete", name)
        if mounted:
            assert os.path.samefile(storage, args.root / "store")
            run("umount", str(storage))
        storage.rmdir()
        assert run("systemctl", "show", "firecrab-api", "-p", "MainPID", "--value") == primary
        assert run("systemctl", "list-units", "--type=service", "--no-legend", "firecrab-vm-*") == before
        print("PASS pre-existing API PID and workload units preserved", flush=True)


if __name__ == "__main__":
    main()
