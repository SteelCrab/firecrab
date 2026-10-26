#!/usr/bin/env python3
"""Run the production shell with real Linux processes in an owned root fixture.

This is a process/tooling fixture. It does not validate a MicroVM, init system,
container engine, provider, network policy or public preview.
"""
import base64
import io
import json
import os
import pwd
from pathlib import Path
import re
import shutil
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
    sync_script = re.search(r'const SYNC_FILESYSTEMS: &str = r#"(.*?)"#;', source, re.S).group(1)
    synced = subprocess.run(["/bin/sh", "-s"], input=sync_script.encode(), capture_output=True, timeout=15)
    assert synced.returncode == 0 and synced.stdout == b"firecrab_sync_v1\n", synced
    # Fault both implementations. A shell function cannot intercept the absolute
    # BusyBox executable selected by prepared guests.
    failed_sync_script = sync_script.replace("/etc/firecrab/busybox sync\n", "(exit 12)\n").replace("; then sync\n", "; then (exit 12)\n")
    assert failed_sync_script != sync_script
    failed_sync = subprocess.run(["/bin/sh", "-s"], input=failed_sync_script.encode(),
                                 capture_output=True, timeout=15)
    assert failed_sync.returncode != 0 and b"firecrab_sync_v1" not in failed_sync.stdout, failed_sync
    capability_script = re.search(r'const CAPABILITIES: &str = r#"(.*?)"#;', source, re.S).group(1)
    capabilities = subprocess.run(["/bin/sh", "-s"], input=capability_script.encode(),
                                  capture_output=True, timeout=15)
    assert capabilities.returncode == 0, capabilities.stderr
    assert b"execution=true\n" in capabilities.stdout, capabilities.stdout
    shell = re.search(r'const SCRIPT: &str = r#"(.*?)"#;', source, re.S).group(1)
    shell = shell.replace("if command -v setpriv", "if false && command -v setpriv")
    shell = shell.replace("if command -v flock", "if false && command -v flock")
    busybox_accounts = os.environ.get("FIRECRAB_SANDBOX_BUSYBOX_ACCOUNTS") == "1"
    if busybox_accounts:
        assert Path("/etc/firecrab/busybox").is_file()
        shell = shell.replace("if command -v useradd", "if false && command -v useradd")
        shell = shell.replace("elif command -v adduser", "elif false && command -v adduser")
        shell = shell.replace("if command -v userdel", "if false && command -v userdel")
        shell = shell.replace("if command -v groupadd", "if false && command -v groupadd")
        shell = shell.replace("elif command -v addgroup", "elif false && command -v addgroup")
        shell = shell.replace("if command -v groupdel", "if false && command -v groupdel")
        shell = shell.replace("elif command -v delgroup", "elif false && command -v delgroup")
    subprocess.run(["/usr/local/sbin/firecrab-console-resize", "--sandbox-probe"], check=True)
    checks = ["confirmed_filesystem_sync", "actual_capability_tar_probe"]
    with tempfile.TemporaryDirectory(prefix="firecrab-guest-shell-") as temporary:
        directory = Path(temporary)
        directory.chmod(0o755)
        root = directory / "guest"
        sentinel = directory / "unrelated"
        sentinel.write_text("preserve")

        def program(action, attempt, invocation, payload=b"", seconds=5, persistent=False, workspace=None, mode="direct", output=65536, fail_group=False, fail_create=False, fail_before_user=False, fail_before_ack=False):
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
            if fail_group:
                text = text.replace('[ "$(id -gn "$user")" = "$user" ] || exit 72', "exit 72")
            if fail_create:
                create = 'tool adduser -D -H -G "$user" -h "$work" -s /bin/sh "$user"'
                text = text.replace(create + " || created=false", "{ " + create + "; false; } || created=false")
            if fail_before_user:
                for create in ['useradd -M -g "$user" -d "$work" -s /bin/sh "$user"', 'tool adduser -D -H -G "$user" -h "$work" -s /bin/sh "$user"', 'adduser -D -H -G "$user" -h "$work" -s /bin/sh "$user"']:
                    text = text.replace(create + " || created=false", "created=false")
            if fail_before_ack:
                text = text.replace('printf \'%s\' "$id:$gid:$boot" >"$control/group-created.pending"', "exit 72")
            return text.encode()

        def run(action, attempt, invocation, payload=b"", expected=0, **options):
            result = subprocess.run(["/bin/sh", "-s"], input=program(action, attempt, invocation, payload, **options), capture_output=True, timeout=65)
            assert result.returncode == expected, (action, result.returncode, result.stderr.decode(errors="replace"))
            return result

        if busybox_accounts:
            bb = "/etc/firecrab/busybox"
            # Legacy adduser leaves this app UID with an unregistered primary GID.
            # New task groups must not capture that GID or change its account.
            assert subprocess.run([bb, "adduser", "-D", "-H", "-h", "/unrelated", "-s", "/bin/sh", "fixture-unrelated"], capture_output=True).returncode == 0
            original_passwd = Path("/etc/passwd").read_bytes()
            original_group = Path("/etc/group").read_bytes()
            unrelated = pwd.getpwnam("fixture-unrelated")
            assert not any(line.split(":")[2] == str(unrelated.pw_gid) for line in original_group.decode().splitlines())

            partial, partial_command = uuid.uuid4(), uuid.uuid4()
            run("stage", partial, partial_command, archive({"partial.txt": b"owned"}), expected=72, fail_group=True)
            user = "a" + partial.hex[:30]
            actual = pwd.getpwnam(user)
            assert actual.pw_gid != unrelated.pw_gid
            assert (root / partial.hex / "control/uid").read_text() == str(actual.pw_uid)
            run("cleanup", partial, partial_command)

            partial, partial_command = uuid.uuid4(), uuid.uuid4()
            run("stage", partial, partial_command, archive({}), expected=72, fail_create=True)
            assert (root / partial.hex / "control/uid").read_text() == str(pwd.getpwnam("a" + partial.hex[:30]).pw_uid)
            run("cleanup", partial, partial_command)

            preexisting, preexisting_command = uuid.uuid4(), uuid.uuid4()
            user = "a" + preexisting.hex[:30]
            subprocess.run([bb, "addgroup", "-g", "31001", user], check=True)
            subprocess.run([bb, "adduser", "-D", "-H", "-G", user, user], check=True)
            before = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
            run("stage", preexisting, preexisting_command, archive({}), expected=72)
            run("cleanup", preexisting, preexisting_command, expected=72)
            assert before == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
            assert not (root / preexisting.hex / "control/uid").exists()
            subprocess.run([bb, "deluser", user], check=True, capture_output=True)
            assert original_passwd == Path("/etc/passwd").read_bytes()
            assert original_group == Path("/etc/group").read_bytes()
            checks.append("busybox_explicit_group_partial_uid_checkpoint_preexisting_refusal")

        baseline = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        orphan, orphan_command = uuid.uuid4(), uuid.uuid4()
        run("stage", orphan, orphan_command, archive({}), expected=72, fail_before_user=True)
        control = root / orphan.hex / "control"
        assert (control / "group-created").exists() and not (control / "uid").exists()
        # A confirmed, unused owned group can be cancelled/exported/cleaned without a user.
        run("cancel", orphan, orphan_command)
        run("export", orphan, orphan_command, expected=79)
        run("cleanup", orphan, orphan_command)
        assert not (root / orphan.hex).exists()
        assert baseline == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        checks.append("confirmed_group_only_failure_cancel_export_cleanup")

        def remove_fixture_group(name):
            executable = shutil.which("groupdel") or shutil.which("delgroup")
            command = [executable, name] if executable else ["/etc/firecrab/busybox", "delgroup", name]
            subprocess.run(command, check=True)

        uncertain, uncertain_command = uuid.uuid4(), uuid.uuid4()
        run("stage", uncertain, uncertain_command, archive({}), expected=72, fail_before_ack=True)
        assert not (root / uncertain.hex / "control/group-created").exists()
        before = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        run("cancel", uncertain, uncertain_command, expected=72)
        run("cleanup", uncertain, uncertain_command, expected=72)
        assert before == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        remove_fixture_group("a" + uncertain.hex[:30])

        preexisting_group, preexisting_group_command = uuid.uuid4(), uuid.uuid4()
        group_name = "a" + preexisting_group.hex[:30]
        create_group = [shutil.which("groupadd"), "-g", "31002", group_name] if shutil.which("groupadd") else ["/etc/firecrab/busybox", "addgroup", "-g", "31002", group_name]
        subprocess.run(create_group, check=True)
        before = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        run("stage", preexisting_group, preexisting_group_command, archive({}), expected=72)
        run("cleanup", preexisting_group, preexisting_group_command, expected=72)
        assert before == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        assert not (root / preexisting_group.hex / "control/group-created").exists()
        remove_fixture_group(group_name)

        guarded, guarded_command = uuid.uuid4(), uuid.uuid4()
        run("stage", guarded, guarded_command, archive({}), expected=72, fail_before_user=True)
        control = root / guarded.hex / "control"
        acknowledgement = (control / "group-created").read_bytes()
        original_gid = (control / "gid").read_bytes()
        (control / "gid").write_text(str(int(original_gid) + 1))
        before = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        run("cleanup", guarded, guarded_command, expected=72)
        assert before == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        (control / "gid").write_bytes(original_gid)
        (control / "group-created").write_bytes(b"foreign-owner")
        before = (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        run("cleanup", guarded, guarded_command, expected=72)
        assert before == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        (control / "group-created").write_bytes(acknowledgement)
        (control / "group-created").chmod(0o644)
        run("cleanup", guarded, guarded_command, expected=72)
        (control / "group-created").chmod(0o600)
        owned_group = "a" + guarded.hex[:30]
        groups = Path("/etc/group").read_text()
        Path("/etc/group").write_text("\n".join(line + "root" if line.startswith(owned_group + ":") else line for line in groups.splitlines()) + "\n")
        changed = Path("/etc/group").read_bytes()
        run("cleanup", guarded, guarded_command, expected=72)
        assert Path("/etc/group").read_bytes() == changed
        Path("/etc/group").write_text(groups)
        passwd = Path("/etc/passwd").read_text()
        Path("/etc/passwd").write_text(passwd + "fixture-foreign-primary:x:31003:" + original_gid.decode() + ":fixture:/unrelated:/bin/sh\n")
        changed = Path("/etc/passwd").read_bytes()
        run("cleanup", guarded, guarded_command, expected=72)
        assert Path("/etc/passwd").read_bytes() == changed
        Path("/etc/passwd").write_text(passwd)
        run("cleanup", guarded, guarded_command)
        assert baseline == (Path("/etc/passwd").read_bytes(), Path("/etc/group").read_bytes())
        checks.append("group_creation_unacknowledged_foreign_owner_mode_and_members_preserved")

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

        build_workspace, build_attempt = uuid.uuid4(), uuid.uuid4()
        first_build, second_build = uuid.uuid4(), uuid.uuid4()
        build_options = {"persistent": True, "workspace": build_workspace}
        sources = {"Makefile": b"target/main: src/main.c\n\tmkdir -p target\n\tcc src/main.c -o target/main\n",
                   "src/main.c": b'#include <stdio.h>\nint main(void) { puts("first"); return 0; }\n'}
        run("stage", build_attempt, first_build, archive(sources), **build_options)
        build = b"set -eu; make -s target/main >/dev/null; ./target/main"
        assert run("run", build_attempt, first_build, build, **build_options).stdout == b"first\n"
        binary = root / ".workspaces" / build_workspace.hex / "target/main"
        first_binary_mtime = binary.stat().st_mtime_ns
        sources["src/main.c"] = b'#include <stdio.h>\nint main(void) { puts("second"); return 0; }\n'
        # archive() produces zero mtimes, exactly like the host file-tool snapshot.
        run("stage", build_attempt, second_build, archive(sources), **build_options)
        source_path = binary.parent.parent / "src/main.c"
        rebuilt = run("run", build_attempt, second_build, build, **build_options).stdout
        assert rebuilt == b"second\n", rebuilt
        assert source_path.stat().st_mtime_ns > first_binary_mtime
        run("cleanup", build_attempt, second_build, **build_options)
        checks.append("host_file_edit_invalidates_existing_incremental_build")

        # Exercise the production directory creation, then traverse it as a real
        # non-root UID. This is a filesystem proof, not an nspawn engine mock.
        nspawn_parent = directory / "nspawn-root-mode"
        nspawn_parent.mkdir(mode=0o755)
        create_root = re.search(r'^\s+mkdir(?: -m 755)? "\$cmd/rootfs"$', shell, re.M).group(0)
        subprocess.run(["/bin/sh", "-c", 'umask 077; cmd="$1"; ' + create_root,
                        "root-permission-probe", str(nspawn_parent)], check=True)
        private_root = nspawn_parent / "rootfs"
        traversed = subprocess.run(["/usr/local/sbin/firecrab-console-resize", "--reuid", "65534",
                                   "--regid", "65534", "--clear-groups", "--no-new-privs", "/bin/sh", "-c",
                                   'set -eu; test "$(id -u)" = 65534; CDPATH= cd -- "$1"; printf traversable',
                                   "root-permission-probe", str(private_root)], capture_output=True, timeout=5)
        assert traversed.returncode == 0 and traversed.stdout == b"traversable", traversed
        assert private_root.stat().st_uid == 0 and private_root.stat().st_mode & 0o777 == 0o755
        checks.append("nspawn_private_root_traversable_by_actual_payload_uid")
        if busybox_accounts:
            assert original_passwd == Path("/etc/passwd").read_bytes()
            assert original_group == Path("/etc/group").read_bytes()
            checks.append("busybox_accounts_cleanup_preserves_unrelated_app_uid_and_groups")
    print(json.dumps({"kind": "linux_process_fixture", "microvmValidated": False, "checks": checks, "status": "passed"}, indent=2))


if __name__ == "__main__":
    main()
