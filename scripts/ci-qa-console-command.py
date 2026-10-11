#!/usr/bin/env python3
"""Run a QA command through the installed CLI's serial console, without SSH."""
import os
import re
import select
import subprocess
import sys
import time
import uuid

cli, api, vm, command = sys.argv[1:]
token = uuid.uuid4().hex
begin = f"FIRECRAB_QA_BEGIN_{token}"
end = f"FIRECRAB_QA_END_{token}"
payload = (
    f"\x15printf '\\n%s\\n' '{begin}'; ({command}); rc=$?; "
    f"printf '\\n%s:%s\\n' '{end}' \"$rc\"\r"
).encode()
process = subprocess.Popen(
    [cli, "--api", api, "vm", "console", vm],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
)
try:
    process.stdin.write(payload)
    process.stdin.flush()
    output = bytearray()
    deadline = time.monotonic() + float(os.environ.get("FIRECRAB_QA_CONSOLE_TIMEOUT", "10"))
    pattern = re.compile(
        rb"\r?\n" + begin.encode() + rb"\r?\n(.*?)\r?\n" + end.encode() + rb":(\d+)\r?\n",
        re.DOTALL,
    )
    while time.monotonic() < deadline:
        if not select.select([process.stdout], [], [], 0.2)[0]:
            continue
        chunk = os.read(process.stdout.fileno(), 65536)
        if not chunk:
            break
        output.extend(chunk)
        match = pattern.search(output)
        if match:
            sys.stdout.buffer.write(match[1].replace(b"\r\n", b"\n"))
            sys.exit(int(match[2]))
        if len(output) > 512 * 1024:
            del output[: len(output) - 512 * 1024]
    raise SystemExit("serial command did not complete")
finally:
    if process.poll() is None:
        try:
            process.stdin.write(b"\x1d")
            process.stdin.flush()
            process.wait(timeout=3)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            process.kill()
            process.wait()
