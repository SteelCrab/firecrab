#!/usr/bin/env python3
"""G7/G7a: exercise repair on an idle, installed macOS management VM."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import signal
import subprocess
import sys
import time
import urllib.request

REPAIR = "firecrab service repair"
CONFIG_DIGEST = (
    'helper=firecrab-helper; '
    'if [ "$(systemctl show --property=LoadState --value firecrab-helper.service)" = not-found ]; '
    'then helper=firecrab-net-helper; fi; '
    'systemctl cat firecrab-api "$helper" | sha256sum'
)


class QAError(Exception):
    """A prerequisite, assertion, or restoration failed."""


def require(condition, detail):
    if not condition:
        raise QAError(detail)


def require_idle(vms):
    require(isinstance(vms, list), "VM list is not an array")
    for vm in vms:
        require(vm.get("state") in {"created", "stopped", "error"},
                f"VM {vm.get('id', '?')} is {vm.get('state', 'unknown')}; stop workloads first")


def digest(path):
    hashed = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            hashed.update(block)
    return hashed.hexdigest()


def file_identity(path):
    stat = path.stat()
    return [stat.st_dev, stat.st_ino, stat.st_size]


def snapshot(home, install):
    immutable = [install / "firecrab", install / "firecrab-micromanager-macos"]
    immutable += [home / file for file in (
        "runtime/manager_ed25519", "runtime/provisioned", "system/Image", "system/initrd.img",
    )]
    for file in ("runtime/manager_ed25519.pub", "runtime/cloud-init-seed.iso"):
        if (home / file).exists():
            immutable.append(home / file)
    disks = [home / "system/debian-system.raw", home / "data/firecrab-data.raw"]
    if (home / "runtime/efi-variable-store").exists():
        disks.append(home / "runtime/efi-variable-store")
    for path in immutable + disks:
        require(path.is_file() and not path.is_symlink(), f"missing or symlinked asset: {path}")
    known_hosts = home / "runtime/known_hosts"
    require(not known_hosts.is_symlink(), f"symlinked SSH host pins: {known_hosts}")
    return {
        "hashes": {str(path): digest(path) for path in immutable},
        "disks": {str(path): file_identity(path) for path in disks},
        "downloads": {
            str(path.relative_to(home)): file_identity(path) + [path.stat().st_mtime_ns]
            for path in (home / "downloads").rglob("*") if path.is_file()
        },
        "known_hosts": set(known_hosts.read_text().splitlines()) if known_hosts.exists() else set(),
    }


def assert_preserved(before, after):
    for field in ("hashes", "disks", "downloads"):
        require(before[field] == after[field], f"repair changed preserved {field}")
    require(before["known_hosts"] <= after["known_hosts"], "repair removed pinned SSH host keys")


class QA:
    def __init__(self, cli, home, install, results):
        self.cli, self.home, self.install, self.results = cli, home, install, results
        self.plist = Path.home() / "Library/LaunchAgents/io.firecrab.micromanager.plist"
        self.wrapper = home / "runtime/daemon.sh"
        self.ready = home / "runtime/daemon-ready"
        self.backups = {}
        self.mutated = False
        self.config_before = None
        self.summary = {
            "status": "RUNNING", "platform": platform.platform(), "cli": str(cli),
            "managed_home": str(home), "install_dir": str(install), "checks": [], "warnings": [],
        }
        results.mkdir(parents=True, exist_ok=False, mode=0o700)
        self.log = (results / "run.log").open("w")

    def run(self, args, check=True, timeout=240, log_output=True):
        self.log.write(f"$ {shlex.join([str(arg) for arg in args])}\n")
        self.log.flush()
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        if log_output:
            self.log.write(result.stdout + result.stderr)
        self.log.write(f"exit={result.returncode}\n")
        self.log.flush()
        require(not check or result.returncode == 0, f"command failed ({result.returncode}): {args}")
        return result

    def service(self, *args, **kwargs):
        return self.run([self.cli, "service", *args], **kwargs)

    def api(self, route):
        with urllib.request.urlopen("http://127.0.0.1:5523" + route, timeout=5) as response:
            require(response.status == 200, f"HTTP {response.status}: {route}")
            return json.load(response)

    def catalogs(self):
        return {
            route: sorted(str(row.get("id", row.get("alias", ""))) for row in self.api(route))
            for route in ("/api/vms", "/api/micro-networks", "/api/images")
        }

    def config_digest(self):
        return self.service("shell", "--", "bash", "-o", "pipefail", "-ec", CONFIG_DIGEST).stdout.strip()

    def marker(self):
        return dict(line.split("=", 1) for line in self.ready.read_text().splitlines() if "=" in line)

    def check(self, item, name, operation):
        entry = {"id": item, "name": name, "status": "RUNNING"}
        self.summary["checks"].append(entry)
        try:
            operation()
        except (Exception, KeyboardInterrupt) as error:
            entry.update(status="FAILED", detail=str(error) or "interrupted")
            print(f"FAILED {item}: {name}: {error}", flush=True)
            raise
        entry["status"] = "PASS"
        print(f"PASS {item}: {name}", flush=True)

    def healthy(self):
        self.service("status")
        self.api("/api/host")
        self.service("shell", "--", "systemctl", "is-active", "firecrab-api")
        require(self.catalogs() == self.catalogs_before, "persistent API catalogs changed")
        assert_preserved(self.files_before, snapshot(self.home, self.install))
        current = self.config_digest()
        if self.config_before is None:
            self.config_before = current
        require(current == self.config_before, "guest service configuration/development overrides changed")

    def record_snapshot(self, name, files):
        evidence = dict(files)
        evidence["known_hosts"] = sorted(
            hashlib.sha256(line.encode()).hexdigest() for line in files["known_hosts"]
        )
        (self.results / name).write_text(json.dumps(evidence, indent=2) + "\n")

    def gate(self):
        require(self.home.is_absolute() and self.install.is_absolute(), "managed paths must be absolute")
        for path in (self.home, self.install, self.plist.parent):
            require(not any(parent.is_symlink() for parent in [path, *path.parents]),
                    f"refusing a symlinked managed/registration path: {path}")
        source = Path(__file__).resolve().parents[1]
        self.summary["revision"] = self.run(
            ["git", "-C", str(source), "-c", "core.fsmonitor=false", "rev-parse", "HEAD"],
        ).stdout.strip()
        self.summary["dirty"] = bool(self.run(
            ["git", "-C", str(source), "-c", "core.fsmonitor=false", "status", "--porcelain"],
        ).stdout.strip())
        require_idle(self.api("/api/vms"))
        self.service("status")
        job = self.run(["/bin/launchctl", "print", f"gui/{os.getuid()}/io.firecrab.micromanager"],
                       log_output=False)
        require(str(self.wrapper) in job.stdout, "loaded launchd job belongs to another managed home")
        self.files_before = snapshot(self.home, self.install)
        self.record_snapshot("preservation-before.json", self.files_before)
        self.catalogs_before = self.catalogs()
        backup = self.results / "registration-backup"
        backup.mkdir(mode=0o700)
        for path in (self.plist, self.wrapper):
            require(path.is_file() and not path.is_symlink(), f"missing registration: {path}")
            target = backup / path.name
            shutil.copy2(path, target)
            self.backups[path] = target
        try:
            self.config_before = self.config_digest()
        except QAError:
            self.summary["warnings"].append({
                "id": "G7", "detail": "initial SSH unavailable; guest-config preservation starts after first repair",
            })

    def restart(self):
        self.mutated = True
        self.service("stop")
        self.service("repair")
        self.healthy()

    def lost_plist(self):
        pid = self.marker()["vm_pid"]
        self.plist.unlink()
        output = self.service("start", check=False)
        require(output.returncode != 0 and REPAIR in output.stderr, "start did not suggest repair")
        require(not self.plist.exists() and self.marker()["vm_pid"] == pid, "start repaired automatically")
        self.api("/api/host")
        self.service("repair")
        require(self.plist.is_file(), "repair did not recreate the plist")
        self.healthy()

    def lost_wrapper(self):
        self.service("stop")
        self.wrapper.unlink()
        output = self.service("start", check=False)
        require(output.returncode != 0 and REPAIR in output.stderr, "missing wrapper did not suggest repair")
        require(not self.wrapper.exists(), "start recreated the wrapper automatically")
        self.service("repair")
        require(self.wrapper.is_file(), "repair did not recreate the wrapper")
        self.healthy()

    def ssh_failure(self):
        original = self.ready.read_bytes()
        marker = self.marker()
        marker["ip"] = "192.0.2.254"
        try:
            self.ready.write_text("".join(f"{key}={value}\n" for key, value in marker.items()))
            output = self.service("dev", "--restore", check=False, timeout=20)
            require(output.returncode != 0 and REPAIR in output.stderr, "dev SSH failure did not suggest repair")
            require("source upload was not attempted" in output.stderr, "SSH failed after deployment started")
            require("[GUEST]" not in output.stdout, "dev reached guest deployment after SSH failure")
            require(self.marker()["vm_pid"] == marker["vm_pid"], "dev repaired automatically")
        finally:
            self.ready.write_bytes(original)
        self.service("repair")
        self.healthy()

    def tunnel_failure(self):
        pid = int(self.marker()["tunnel_pid"])
        process = self.run(["/bin/ps", "-p", str(pid), "-o", "uid=,command="], log_output=False).stdout.strip()
        uid, command = process.split(None, 1)
        require(int(uid) == os.getuid() and "ssh" in command and "5523" in command
                and str(self.home / "runtime/manager_ed25519") in command,
                "tunnel PID is not this user's managed SSH process")
        os.kill(pid, signal.SIGSTOP)
        try:
            output = self.service("status", check=False, timeout=15)
            require(output.returncode != 0 and REPAIR in output.stdout, "unreachable API did not suggest repair")
            report = json.loads(self.service("debug", "--json").stdout)
            require(REPAIR in report["api"]["detail"], "JSON diagnostics have no recovery advice")
        finally:
            os.kill(pid, signal.SIGCONT)
        self.service("repair")
        self.healthy()

    def restore(self):
        self.service("stop", check=False, timeout=60)
        for destination, source in self.backups.items():
            shutil.copy2(source, destination)
        self.service("start")
        self.service("status")
        self.api("/api/host")

    def execute(self):
        try:
            self.check("G7", "idle installed-runtime gate", self.gate)
            self.check("G7", "stopped-service repair and preservation", self.restart)
            self.check("G7", "loaded service with missing plist", self.lost_plist)
            self.check("G7", "missing daemon wrapper", self.lost_wrapper)
            self.check("G7a", "dev SSH failure advice and recovery", self.ssh_failure)
            self.check("G7a", "API tunnel failure and JSON advice", self.tunnel_failure)
            self.summary["status"] = "PASS"
        except (Exception, KeyboardInterrupt) as error:
            self.summary.update(status="FAILED", error=str(error) or "interrupted")
            if self.mutated:
                try:
                    self.restore()
                    self.summary["restoration"] = "PASS"
                except Exception as restore_error:
                    self.summary["restoration"] = f"FAILED: {restore_error}"
        finally:
            if hasattr(self, "files_before"):
                try:
                    files_after = snapshot(self.home, self.install)
                    self.record_snapshot("preservation-after.json", files_after)
                    assert_preserved(self.files_before, files_after)
                    self.summary["preservation"] = "PASS"
                except Exception as error:
                    self.summary["preservation"] = f"FAILED: {error}"
                    if self.summary["status"] == "PASS":
                        self.summary.update(status="FAILED", error=str(error))
            self.log.close()
            (self.results / "summary.json").write_text(json.dumps(self.summary, indent=2) + "\n")
        print(f"{self.summary['status']}: evidence at {self.results}", flush=True)
        return 0 if self.summary["status"] == "PASS" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", default="firecrab")
    parser.add_argument("--results-dir", type=Path)
    args = parser.parse_args()
    require(platform.system() == "Darwin", "repair runtime QA requires macOS")
    cli = shutil.which(args.cli)
    require(cli is not None, f"CLI not found: {args.cli}")
    home = Path(os.environ.get("FIRECRAB_MICROMANAGER_HOME", str(Path.home() / "Library/Application Support/Firecrab/micromanager")))
    install = Path(os.environ.get("FIRECRAB_INSTALL_DIR", str(Path.home() / ".local/bin")))
    results = args.results_dir or Path(__file__).resolve().parents[1] / "target/qa/macos-repair" / time.strftime("%Y%m%d-%H%M%S")
    os.umask(0o077)
    # Let the normal failure path restore injected state on termination too.
    def terminate(_signum, _frame):
        raise KeyboardInterrupt("terminated")
    signal.signal(signal.SIGTERM, terminate)
    return QA(cli, home, install, results).execute()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (QAError, OSError) as error:
        print(f"FAILED G7: {error}", file=sys.stderr)
        sys.exit(1)
