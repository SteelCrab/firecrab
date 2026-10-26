#!/usr/bin/env python3
"""Run the production shell with real Linux processes in an owned root fixture.

This is a process/tooling fixture. It does not validate a MicroVM, init system,
container engine, provider, network policy or public preview.
"""
import base64
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]


def archive(files):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as stream:
        for name, data in files.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(data)
            entry.mode = 0o600
            stream.addfile(entry, io.BytesIO(data))
    return output.getvalue()


def contents(data):
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as stream:
        return {entry.name.removeprefix("./"): stream.extractfile(entry).read()
                for entry in stream.getmembers() if entry.isfile()}


def main():
    assert os.geteuid() == 0 and os.environ.get("FIRECRAB_SANDBOX_GUEST_QA") == "1", "disposable Linux root fixture only"
    source = (ROOT / "firecrab-api/src/ai_sandbox/guest.rs").read_text()
    capability_script = re.search(r'const CAPABILITIES: &str = r#"(.*?)"#;', source, re.S).group(1)
    capabilities = subprocess.run(["/bin/sh", "-s"], input=capability_script.encode(),
                                  capture_output=True, timeout=15)
    assert capabilities.returncode == 0, capabilities.stderr
    assert b"execution=true\n" in capabilities.stdout, capabilities.stdout
    shell = re.search(r'const SCRIPT: &str = r#"(.*?)"#;', source, re.S).group(1)
    shell = shell.replace("if command -v setpriv", "if false && command -v setpriv")
    shell = shell.replace("if command -v flock", "if false && command -v flock")
    subprocess.run(["/usr/local/sbin/firecrab-console-resize", "--sandbox-probe"], check=True)
    checks = ["actual_capability_tar_probe"]
    with tempfile.TemporaryDirectory(prefix="firecrab-guest-shell-") as temporary:
        directory = Path(temporary)
        directory.chmod(0o755)
        root = directory / "guest"
        sentinel = directory / "unrelated"
        sentinel.write_text("preserve")

        def program(action, attempt, invocation, payload=b"", seconds=5, persistent=False, workspace=None, mode="direct", output=65536):
            values = {"__ID__": attempt.hex, "__COMMAND__": invocation.hex, "__ACTION__": action,
                      "__PAYLOAD__": base64.b64encode(payload).decode(), "__SECONDS__": str(seconds),
                      "__MODE__": mode, "__IMAGE__": "example.invalid/test@sha256:" + "a" * 64,
                      "__PERSISTENT__": str(persistent).lower(), "__WORKSPACE__": (workspace or attempt).hex,
                      "__CPU_SECONDS__": "5", "__MEMORY_KIB__": "131072", "__OUTPUT_BYTES__": str(output)}
            text = shell.replace("/var/lib/firecrab-ai", str(root))
            for name, value in values.items():
                text = text.replace(name, value)
            if action == "signal":
                text = text.replace(base64.b64encode(payload).decode(), payload.decode())
            return text.encode()

        def run(action, attempt, invocation, payload=b"", expected=0, **options):
            result = subprocess.run(["/bin/sh", "-s"], input=program(action, attempt, invocation, payload, **options), capture_output=True, timeout=65)
            assert result.returncode == expected, (action, result.returncode, result.stderr.decode(errors="replace"))
            return result

        attempt, invocation = uuid.uuid4(), uuid.uuid4()
        run("stage", attempt, invocation, archive({"main.txt": b"initial"}))
        command = b'set -eu; test "$(id -u)" -ne 0; test "$(awk \'/^NoNewPrivs:/ {print $2}\' /proc/self/status)" = 1; test -z "${OPENAI_API_KEY-}"; test "$(ulimit -n)" -eq 1024; test "$(ulimit -Hn)" -eq 1024; echo changed >main.txt; printf stdout; printf stderr >&2; exit 7'
        result = run("run", attempt, invocation, command, expected=7)
        assert result.stdout == b"stdout" and result.stderr == b"stderr"
        assert run("status", attempt, invocation).stdout == b"7"
        assert contents(run("export", attempt, invocation).stdout)["main.txt"] == b"changed\n"
        checks.append("static_uid_lock_stdout_stderr_exit_export")
        overflow = run("run", attempt, uuid.uuid4(), b"yes overflow", expected=125, output=8)
        assert len(overflow.stdout) == 8
        checks.append("bounded_output_stops_owned_uid")

        invocation = uuid.uuid4()
        process = subprocess.Popen(["/bin/sh", "-s"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        process.stdin.write(program("run", attempt, invocation, b"touch started; echo partial; sleep 30", seconds=40))
        process.stdin.close()
        process.stdin = None
        try:
            marker = root / attempt.hex / "workspace/started"
            for _ in range(100):
                if marker.exists():
                    break
                assert process.poll() is None
                time.sleep(0.02)
            assert marker.exists()
            owned = run("processes", attempt, invocation).stdout.decode().splitlines()
            assert owned
            pid, start, boot, _ = owned[0].split()
            refused = f"{pid}:{start}:{uuid.uuid4()}:KILL".encode()
            run("signal", attempt, invocation, refused, expected=77)
            run("cancel", attempt, invocation)
            process.communicate(timeout=10)
            recovered = run("recover", attempt, invocation)
            assert recovered.stdout.startswith(b"partial\n")
            run("export", attempt, invocation)
            run("cleanup", attempt, invocation)
            assert not (root / attempt.hex).exists()
            checks.append("process_identity_refusal_cancel_recover_cleanup")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

        workspace = uuid.uuid4()
        first, first_command = uuid.uuid4(), uuid.uuid4()
        options = {"persistent": True, "workspace": workspace}
        run("stage", first, first_command, archive({"main.txt": b"initial"}), **options)
        run("run", first, first_command, b"mkdir -p .local/pkg .config config; echo package >.local/pkg/installed; echo private >.config/tool; echo delete >config/tool.conf; echo retained >main.txt", **options)
        run("export", first, first_command, **options)
        run("cleanup", first, first_command, **options)
        second, second_command = uuid.uuid4(), uuid.uuid4()
        files = contents(run("hydrate", second, second_command, **options).stdout)
        assert files["main.txt"] == b"retained\n" and ".config/tool" not in files
        baseline = ("\n".join(files) + "\n").encode()
        run("hydrate_baseline", second, second_command, baseline, **options)
        files["main.txt"] = b"host-overlay\n"
        del files["config/tool.conf"]
        run("stage", second, second_command, archive(files), **options)
        command = b'set -eu; test "$(cat main.txt)" = host-overlay; test ! -e config/tool.conf; test "$(cat .local/pkg/installed)" = package; test "$(cat .config/tool)" = private; printf retained'
        assert run("run", second, second_command, command, **options).stdout == b"retained"
        # Final host file edit after the command must survive the final flush/export.
        files["main.txt"] = b"final-file-tool\n"
        run("stage", second, second_command, archive(files), **options)
        assert contents(run("export", second, second_command, **options).stdout)["main.txt"] == b"final-file-tool\n"
        run("cleanup", second, second_command, **options)
        assert (root / ".workspaces" / workspace.hex / "main.txt").read_bytes() == b"final-file-tool\n"
        assert sentinel.read_text() == "preserve"
        checks.append("workspace_hydrate_overlay_delete_private_retention_final_flush")
    print(json.dumps({"kind": "linux_process_fixture", "microvmValidated": False, "checks": checks, "status": "passed"}, indent=2))


if __name__ == "__main__":
    main()
