#!/usr/bin/env python3
"""Real KVM pause/resume QA in a private network namespace and temporary API.

Run as root on a disposable Linux/KVM test host. Installed Firecrab services,
images, network namespaces, and workload units are preserved.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT).strip()


def until(check, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError("timed out waiting for QA condition")


class Console:
    def __init__(self, port, vm):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=5)
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall((f"GET /ws/vms/{vm}/console HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            header += self.socket.recv(1)
        assert header.startswith(b"HTTP/1.1 101"), header
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        assert accept in header
        self.text = ""

    def send(self, text):
        data = text.encode()
        mask = os.urandom(4)
        size = bytes([0x80 | len(data)]) if len(data) < 126 else bytes([0x80 | 126]) + struct.pack("!H", len(data))
        self.socket.sendall(b"\x81" + size + mask + bytes(value ^ mask[i % 4] for i, value in enumerate(data)))

    def exact(self, count):
        data = b""
        while len(data) < count:
            chunk = self.socket.recv(count - len(data))
            if not chunk:
                raise EOFError("console closed")
            data += chunk
        return data

    def read(self, seconds=0.2):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if not select.select([self.socket], [], [], max(0, deadline - time.monotonic()))[0]:
                break
            first, size = self.exact(2)
            size &= 127
            if size == 126:
                size = struct.unpack("!H", self.exact(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.exact(8))[0]
            data = self.exact(size)
            if first & 15 in (1, 2):
                self.text += data.decode(errors="replace")
            elif first & 15 == 8:
                raise EOFError("console closed")
        return re.findall(r"FC442:([a-f0-9-]+):(\d+):([a-f0-9-]+):(\d+)", self.text)

    def sample(self):
        values = self.read()
        return values[-1] if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-binary", type=Path, required=True)
    parser.add_argument("--helper-binary", type=Path, required=True)
    parser.add_argument("--image-root", type=Path, required=True)
    parser.add_argument("--template", required=True, help="Installed HTTP image with a serial root shell")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--inside", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    assert os.geteuid() == 0 and Path("/dev/kvm").is_char_device(), "requires root and KVM"
    if args.inside:
        exercise(args)
        return
    name = "fc442-" + uuid.uuid4().hex[:10]
    root = Path(tempfile.mkdtemp(prefix="q", dir="/opt"))
    vm_id = None
    namespace_created = False
    dropin = None
    report = args.report.resolve()
    before = run("systemctl", "list-units", "--type=service", "--no-legend", "firecrab-vm-*")
    try:
        for source, target in [(args.api_binary, "firecrab-api"), (args.helper_binary, "firecrab-helper")]:
            shutil.copy2(source, root / target)
        (root / "run").mkdir()
        images = root / "images"
        images.mkdir()
        entries = json.loads((args.image_root / ".templates.json").read_text())
        entry = next(item for item in entries if item["alias"] == args.template)
        for field in ("kernel", "initrd", "rootfs"):
            if entry.get(field):
                relative = Path(entry[field])
                assert not relative.is_absolute() and ".." not in relative.parts
                destination = images / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(args.image_root / relative, destination)
        (images / ".templates.json").write_text(json.dumps([entry]))
        run("ip", "netns", "add", name)
        namespace_created = True
        run("ip", "-n", name, "link", "set", "lo", "up")
        # A private dummy uplink gives the helper an uplink identity without host traffic.
        run("ip", "-n", name, "link", "add", "qa-uplink", "type", "dummy")
        run("ip", "-n", name, "addr", "add", "192.0.2.1/24", "dev", "qa-uplink")
        run("ip", "-n", name, "link", "set", "qa-uplink", "up")
        run("ip", "-n", name, "route", "add", "default", "dev", "qa-uplink")
        command = ["ip", "netns", "exec", name, "python3", str(Path(__file__).resolve()), "--api-binary", str(root / "firecrab-api"), "--helper-binary", str(root / "firecrab-helper"), "--image-root", str(images), "--template", args.template, "--report", str(report), "--inside", str(root)]
        subprocess.run(command, check=True)
    finally:
        marker = root / "vm-id"
        if marker.exists():
            vm_id = uuid.UUID(marker.read_text())
            unit = f"firecrab-vm-{vm_id.hex}.service"
            subprocess.run(["systemctl", "stop", unit], check=False, capture_output=True)
            subprocess.run(["systemctl", "reset-failed", unit], check=False, capture_output=True)
            dropin = Path("/run/systemd/system") / (unit + ".d")
            shutil.rmtree(dropin, ignore_errors=True)
            subprocess.run(["systemctl", "daemon-reload"], check=False, capture_output=True)
        if namespace_created:
            pids = subprocess.run(["ip", "netns", "pids", name], text=True, capture_output=True).stdout.split()
            for pid in pids:
                try:
                    os.kill(int(pid), 15)
                except ProcessLookupError:
                    pass
            time.sleep(0.2)
            subprocess.run(["ip", "netns", "delete", name], check=False, capture_output=True)
        storage_marker = root / "storage-path"
        if storage_marker.exists():
            shutil.rmtree(storage_marker.read_text(), ignore_errors=True)
        shutil.rmtree(root)
        after = run("systemctl", "list-units", "--type=service", "--no-legend", "firecrab-vm-*")
        assert before == after, "pre-existing VM units changed during isolated QA"


def exercise(args):
    root = args.inside
    os.chdir(root)
    storage = Path(tempfile.mkdtemp(prefix="q", dir="/tmp"))
    (root / "storage-path").write_text(str(storage))
    env = {**os.environ, "FIRECRAB_STORAGE_ROOTS": f"qa={storage}", "FIRECRAB_NET_HELPER_SOCK": str(root / "helper.sock"), "FIRECRAB_IMAGE_ROOT": str(args.image_root), "FIRECRAB_BIND_ADDR": "127.0.0.1:19442"}
    helper_log = (root / "helper.log").open("w")
    api_log = (root / "api.log").open("w")
    # Keep DHCP runtime files private as well as network state.
    helper = subprocess.Popen(["unshare", "--mount", "--propagation", "private", "sh", "-c", 'set -eu; mount --bind "$1/run" /run/firecrab; exec "$1/firecrab-helper"', "qa", str(root)], env=env, stdout=helper_log, stderr=subprocess.STDOUT)
    until(lambda: (root / "helper.sock").exists())
    api = None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    port = 19442
    def request(path, method="GET", body=None, timeout=10):
        data = json.dumps(body).encode() if body is not None else (b"" if method == "POST" else None)
        with opener.open(urllib.request.Request(f"http://127.0.0.1:{port}" + path, data=data, method=method, headers={"Content-Type": "application/json"}), timeout=timeout) as response:
            payload = response.read()
            return json.loads(payload) if payload else None
    def start_api():
        process = subprocess.Popen([str(args.api_binary)], env=env, stdout=api_log, stderr=subprocess.STDOUT)
        def ready():
            assert process.poll() is None, (root / "api.log").read_text()
            try:
                return request("/api/vms") is not None
            except (OSError, urllib.error.URLError):
                return False
        until(ready)
        return process
    evidence = {"architecture": run("uname", "-m"), "firecracker": run("firecracker", "--version"), "checks": []}
    def passed(check):
        evidence["checks"].append(check)
        print("PASS " + check, flush=True)
    try:
        api = start_api()
        network = request("/api/micro-networks", "POST", {"name":"qa-pause", "subnetCidr":"172.218.42.0/24", "internetEnabled":False, "uplink":"qa-uplink"})
        vm = request("/api/vms", "POST", {"name":"qa-pause", "template":args.template, "cpu":1, "ram":128, "diskGb":1, "storageRoot":"qa", "microNetworkId":network["id"]})
        vm_id = vm["id"]
        (root / "vm-id").write_text(vm_id)
        unit = "firecrab-vm-" + uuid.UUID(vm_id).hex + ".service"
        dropin = Path("/run/systemd/system") / (unit + ".d")
        dropin.mkdir()
        ns = run("ip", "netns", "identify", str(os.getpid()))
        (dropin / "qa.conf").write_text(f"[Service]\nNetworkNamespacePath=/run/netns/{ns}\n")
        run("systemctl", "daemon-reload")
        request(f"/api/vms/{vm_id}/start", "POST")
        def running():
            result = request(f"/api/vms/{vm_id}")
            assert result["state"] != "error", request(f"/api/vms/{vm_id}/log")
            return result if result["state"] == "running" else None
        vm = until(running, 180)
        pid = run("systemctl", "show", "-p", "MainPID", "--value", unit)
        children = Path(f"/proc/{pid}/task/{pid}/children").read_text().strip()
        original_identity = (vm["ipv4"], vm["mac"], children)
        def http():
            try:
                with opener.open(f'http://{vm["ipv4"]}:80/', timeout=3) as response:
                    return response.status == 200
            except (OSError, urllib.error.URLError):
                return False
        console = Console(port, vm_id)
        console.send("\n")
        def prompt():
            console.read()
            if "login:" in console.text and "# " not in console.text:
                console.send("root\n")
            return "# " in console.text
        until(prompt, 30)
        console.send("B=/etc/firecrab/busybox; $B mkdir -p /tmp/fc442-www; printf ok >/tmp/fc442-www/index.html; $B httpd -p 80 -h /tmp/fc442-www >/dev/null 2>&1 || true\n")
        until(http)
        passed("guest HTTP before pause")
        console.send("B=/etc/firecrab/busybox; token=$($B cat /proc/sys/kernel/random/uuid); boot=$($B cat /proc/sys/kernel/random/boot_id); n=0; while :; do printf 'FC442:%s:%s:%s:%s\\n' \"$token\" \"$$\" \"$boot\" \"$n\"; n=$((n+1)); $B sleep 1; done\n")
        sample = until(console.sample)
        for cycle in range(3):
            assert request(f"/api/vms/{vm_id}/pause", "POST")["state"] == "paused"
            frozen = console.read(0.5)[-1]
            assert console.read(2)[-1] == frozen
            assert not http()
            assert request("/api/micromanager/activity")["busy"]
            passed(f"cycle {cycle + 1}: counter frozen and Sleepy inhibited")
            if cycle == 1:
                console.socket.close()
                api.terminate()
                api.wait(timeout=10)
                api = start_api()
                assert request(f"/api/vms/{vm_id}")["state"] == "paused"
                console = Console(port, vm_id)
                assert console.read(1)[-1] == frozen
                passed("API restart retained Paused without guest progress")
            resumed = request(f"/api/vms/{vm_id}/resume", "POST")
            assert resumed["state"] == "running"
            assert (resumed["ipv4"], resumed["mac"], Path(f"/proc/{pid}/task/{pid}/children").read_text().strip()) == original_identity
            later = until(lambda: (value if (value := console.sample()) and int(value[3]) > int(frozen[3]) else None))
            assert later[:3] == sample[:3], (sample, later)
            assert http()
            passed(f"cycle {cycle + 1}: RAM token, guest PID, boot ID, VMM PID, IP/MAC, and HTTP preserved")
        assert request(f"/api/vms/{vm_id}/pause", "POST")["state"] == "paused"
        assert request(f"/api/vms/{vm_id}/stop", "POST")["state"] == "stopped"
        passed("Stop from Paused")
        request(f"/api/vms/{vm_id}", "DELETE")
        request(f'/api/micro-networks/{network["id"]}', "DELETE")
        passed("VM and network cleanup")
        args.report.write_text(json.dumps(evidence, indent=2) + "\n")
    except BaseException:
        print((root / "api.log").read_text(), flush=True)
        print((root / "helper.log").read_text(), flush=True)
        raise
    finally:
        if api:
            api.terminate()
            api.wait(timeout=10)
        helper.terminate()
        helper.wait(timeout=10)


if __name__ == "__main__":
    main()
