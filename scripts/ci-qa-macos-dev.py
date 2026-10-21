#!/usr/bin/env python3
"""Reject read-only guest mounts without changing the management VM's mounts.

Requires a healthy, installed macOS management VM and its root SSH access.
Each failure is injected in a private Linux mount namespace. Running services
and the host namespace retain their writable mounts.
"""
import argparse
from pathlib import Path
import shlex
import subprocess
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, default=Path("target/debug/firecrab"))
    args = parser.parse_args()
    cli = str(args.cli.resolve())
    script = (Path(__file__).parent / "micromanager/dev-macos-guest.sh").read_bytes()

    def guest(*command, data=None):
        return subprocess.run([cli, "service", "shell", *command], input=data,
                              capture_output=True, timeout=30)

    def api():
        with urllib.request.urlopen("http://127.0.0.1:5523/api/host", timeout=5) as response:
            assert response.status == 200

    def snapshot():
        result = guest("sh", "-c", 'set -e; helper=firecrab-helper; '
                       'if [ "$(systemctl show -p LoadState --value firecrab-helper)" = not-found ]; '
                       'then helper=firecrab-net-helper; fi; '
                       'systemctl show -p MainPID firecrab-api "$helper"; '
                       "findmnt -n -o TARGET,OPTIONS --target /; "
                       "findmnt -n -o TARGET,OPTIONS --target /var/lib/firecrab")
        if result.returncode:
            raise RuntimeError(result.stderr.decode())
        return result.stdout

    api()
    before = snapshot()
    for mount in ["/", "/var/lib/firecrab", "/usr/local/lib"]:
        for action in ["start", "debug"]:
            target = shlex.quote(mount)
            command = (f"mount --make-rprivate / && mount --bind {target} {target} && "
                       f"mount -o remount,bind,ro {target} && "
                       f"exec bash -s -- {action} 1.97.1 incoming-qa.tar")
            result = guest("unshare", "--mount", "--", "sh", "-c", command, data=script)
            if (result.returncode != 74 or b"not writable" not in result.stderr
                    or b"[PASS]" in result.stdout):
                raise RuntimeError(f"{mount} {action}: exit={result.returncode}\n"
                                   + result.stdout.decode() + result.stderr.decode())
            api()
            if snapshot() != before:
                raise RuntimeError("failure injection changed running services or host mounts")
            print(f"[PASS] read-only {mount}: {action} rejected; API and mounts preserved")


if __name__ == "__main__":
    main()
