#!/usr/bin/env python3
"""Boot an isolated, network-free Firecracker guest and exercise its real ttyS0."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import tty


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("kernel", "helper", "busybox"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--firecracker", default="firecracker")
    parser.add_argument("--expected-release", required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    args.report.parent.mkdir(parents=True, exist_ok=True)
    transcript = bytearray()
    results = []
    with tempfile.TemporaryDirectory(prefix="fcr471-", dir="/tmp") as temporary:
        root = Path(temporary)
        tree = root / "tree"
        for directory in ("bin", "dev", "proc", "sys", "etc", "tmp", "root"):
            (tree / directory).mkdir(parents=True)
        shutil.copyfile(args.busybox, tree / "bin/busybox")
        shutil.copyfile(args.helper, tree / "resize")
        (tree / "bin/busybox").chmod(0o755)
        (tree / "resize").chmod(0o755)
        (tree / "init").write_text("""#!/bin/busybox sh
export PATH=/bin TERM=xterm PS1='FCR_PROMPT> '
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/resize >/dev/null 2>&1 &
exec /bin/busybox init
""")
        (tree / "init").chmod(0o755)
        (tree / "etc/inittab").write_text("::respawn:-/bin/busybox sh\n")
        disk = root / "rootfs.ext4"
        subprocess.run(["mkfs.ext4", "-q", "-F", "-d", str(tree), str(disk), "32M"], check=True)
        vsock = root / "v.sock"
        config = {
            "boot-source": {"kernel_image_path": str(args.kernel.resolve()), "boot_args": "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/init"},
            "drives": [{"drive_id": "rootfs", "path_on_host": str(disk), "is_root_device": True, "is_read_only": False}],
            "machine-config": {"vcpu_count": 1, "mem_size_mib": 256},
            "vsock": {"guest_cid": 3, "uds_path": str(vsock)},
        }
        config_path = root / "fc.json"
        config_path.write_text(json.dumps(config))
        master, slave = pty.openpty()
        tty.setraw(slave)
        with (root / "firecracker.log").open("wb") as log:
            process = subprocess.Popen([args.firecracker, "--no-api", "--config-file", str(config_path)], stdin=slave, stdout=slave, stderr=log)
            os.close(slave)

            def wait_for(marker, start=0, timeout=30):
                deadline = time.monotonic() + timeout
                while marker not in transcript[start:] and time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError(f"Firecracker exited {process.returncode}: {(root / 'firecracker.log').read_text()}")
                    if select.select([master], [], [], 0.1)[0]:
                        transcript.extend(os.read(master, 65536))
                if marker not in transcript[start:]:
                    raise TimeoutError(f"Guest did not produce {marker!r}; serial tail: {bytes(transcript[-2000:])!r}")

            def command(line, marker):
                start = len(transcript)
                os.write(master, line.encode() + b"\n")
                wait_for(marker, start)

            def resize(rows, cols):
                deadline = time.monotonic() + 15
                while True:
                    try:
                        with socket.socket(socket.AF_UNIX) as stream:
                            stream.settimeout(2)
                            stream.connect(str(vsock))
                            stream.sendall(b"CONNECT 47100\n")
                            greeting = bytearray()
                            while not greeting.endswith(b"\n") and len(greeting) < 64:
                                byte = stream.recv(1)
                                if not byte:
                                    raise ConnectionError("Guest listener is not ready")
                                greeting.extend(byte)
                            if not greeting.startswith(b"OK "):
                                raise ConnectionError(f"Bad vsock handshake: {greeting!r}")
                            stream.sendall(b"FCR1" + struct.pack(">HH", rows, cols))
                            reply = bytearray()
                            while len(reply) < 8:
                                part = stream.recv(8 - len(reply))
                                if not part:
                                    raise ConnectionError("Missing acknowledgement")
                                reply.extend(part)
                            assert reply == b"FCA1" + struct.pack(">HH", rows, cols), reply
                            return
                    except (OSError, ConnectionError):
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(0.1)

            try:
                wait_for(b"FCR_PROMPT>")
                command("printf 'FCR_KERNEL_'; /bin/busybox uname -r", b"FCR_KERNEL_" + args.expected_release.encode())
                resize(30, 100)
                command("printf 'FCR_SIZE_'; /bin/busybox stty size", b"FCR_SIZE_30 100")
                results.append("ttyS0 is 30 rows by 100 columns after acknowledged resize")
                # Reap a short-lived child regularly so ash dispatches pending
                # traps without needing terminal input to release a built-in read.
                command("/bin/busybox sh -c 'trap \"printf FCR_WINCH_; /bin/busybox stty size; exit\" WINCH; printf \"FCR_TRAP_%s\\n\" READY; while :; do /bin/busybox sleep 1; done'", b"FCR_TRAP_READY")
                start = len(transcript)
                resize(40, 120)
                wait_for(b"FCR_WINCH_40 120", start)
                wait_for(b"FCR_PROMPT>", start)
                command("printf 'FCR_SIZE_'; /bin/busybox stty size", b"FCR_SIZE_40 120")
                results.append("foreground process receives SIGWINCH and observes 40 rows by 120 columns")
                command("printf 'FCR_VI_%s\\n' READY > /tmp/size; /bin/busybox vi /tmp/size", b"FCR_VI_READY")
                start = len(transcript)
                resize(24, 80)
                wait_for(b"\x1b[24;1H\x1b[K", start)
                start = len(transcript)
                os.write(master, b":q!\r")
                wait_for(b"FCR_PROMPT>", start)
                command("printf 'FCR_AFTER_VI_'; /bin/busybox stty size", b"FCR_AFTER_VI_24 80")
                results.append("vi remains usable across resize and exits back to the shell")
                command("/bin/busybox top -d 1", b"PID")
                start = len(transcript)
                resize(35, 110)
                wait_for(b"PID", start)
                start = len(transcript)
                os.write(master, b"q")
                wait_for(b"FCR_PROMPT>", start)
                command("printf 'FCR_AFTER_TOP_'; /bin/busybox stty size", b"FCR_AFTER_TOP_35 110")
                results.append("top remains usable across resize and exits back to the shell")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                os.close(master)
                args.report.with_suffix(".console.log").write_bytes(transcript)
                args.report.write_text(json.dumps({"kernel": str(args.kernel), "kernelSha256": hashlib.sha256(args.kernel.read_bytes()).hexdigest(), "expectedRelease": args.expected_release, "checks": results, "passed": len(results) == 4}, indent=2) + "\n")
    for result in results:
        print("PASS", result)


if __name__ == "__main__":
    main()
