#!/usr/bin/env python3
"""Exercise the guest build/deploy transaction without a VM, root, or network."""
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tarfile
import tempfile
import unittest

GUEST_SCRIPT = Path(__file__).parent / "micromanager/dev-macos-guest.sh"
STUB = r'''#!/bin/bash
set -eu
name=${0##*/}
case "$name" in
  chmod|rm|install)
    if [ "$1" = --version ]; then exit 0; fi
    if [ "$name" = install ]; then exec /usr/bin/install "$@"; fi
    exec "/bin/$name" "$@"
    ;;
  findmnt)
    path=${!#}
    selected= flags=
    while read -r device mount type options rest; do
      if [[ "$mount" = / || "$path" = "$mount" || "$path" = "$mount/"* ]]; then
        if [ "${#mount}" -gt "${#selected}" ]; then selected=$mount; flags=$options; fi
      fi
    done < "$TEST_ROOT/mounts"
    echo "$flags"
    ;;
  uname) echo "${GUEST_ARCH:-aarch64}" ;;
  cc|pkg-config|cmake|journalctl|sleep) exit 0 ;;
  flock) [ "${LOCK_FAIL:-0}" = 0 ] ;;
  curl) [ "${HEALTH_FAIL:-0}" = 0 ] ;;
  rustup)
    echo "$*" >>"$TEST_ROOT/rustup-events"
    if [ "$1" = toolchain ]; then [ "${TOOLCHAIN_INSTALL_FAIL:-0}" = 0 ]; exit; fi
    [ "$1" = run ] || exit 0
    if [ "$3" = rustc ]; then [ "${TOOLCHAIN_MISSING:-0}" = 0 ]; exit; fi
    [ "${BUILD_FAIL:-0}" = 0 ] || exit 42
    test "$(cat "$TEST_ROOT/cache/source/firecrab-api/src/main.rs")" = 'edited local source'
    fragment="$TEST_ROOT/cache/source/firecrab-api/src/oci/guest/install-packages.sh"
    if [ -f "$fragment" ]; then cp "$fragment" "$TEST_ROOT/compiled-fragment"; fi
    profile=debug
    for arg in "$@"; do [ "$arg" != --release ] || profile=release; done
    target=
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --target ]; then target=$2; break; fi
      shift
    done
    [ "$target" = "${GUEST_ARCH:-aarch64}-unknown-linux-gnu" ]
    mkdir -p "$CARGO_TARGET_DIR/$target/$profile"
    for unit in firecrab-api firecrab-helper; do
      printf '#!/bin/bash\necho built-from-source\n' >"$CARGO_TARGET_DIR/$target/$profile/$unit"
    done
    if [ "${DISK_FAIL_AFTER_BUILD:-0}" = 1 ]; then
      printf '/dev/os / ext4 ro,relatime 0 0\n' >"$TEST_ROOT/mounts"
    fi
    ;;
  systemctl)
    if [ "$*" = 'show --property=LoadState --value firecrab-helper.service' ]; then
      if [ "${LEGACY_HELPER:-0}" = 1 ]; then echo not-found; else echo loaded; fi
      exit 0
    fi
    echo "$*" >>"$TEST_ROOT/events"
    if [ "$*" = 'stop firecrab-api' ] && [ "${STOP_FAIL:-0}" = 1 ]; then exit 44; fi
    if [ "$1" = start ] && [ "$2" != firecrab-api ] && [ "${HELPER_START_FAIL:-0}" = 1 ]; then exit 45; fi
    if [ "$*" = 'start firecrab-api' ] && [ "${START_FAIL:-0}" = 1 ] \
      && [ -f "$TEST_ROOT/units/firecrab-api.service.d/90-firecrab-dev.conf" ]; then exit 43; fi
    ;;
  *) echo "unexpected stub: $name" >&2; exit 1 ;;
esac
'''


class GuestDevelopmentTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="firecrab-dev-test-", dir="/private/tmp" if Path("/private/tmp").exists() else None)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.cache = self.root / "cache"
        self.binaries = self.root / "binaries"
        self.units = self.root / "units"
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.mounts = self.root / "mounts"
        self.mounts.write_text("/dev/os / ext4 rw,relatime 0 0\n")
        stub = self.commands / "stub"
        stub.write_text(STUB)
        stub.chmod(0o755)
        for name in ["findmnt", "chmod", "rm", "install", "uname", "cc", "pkg-config", "cmake", "journalctl", "sleep", "flock", "curl", "rustup", "systemctl"]:
            (self.commands / name).symlink_to(stub)
        # Linux mv -T replaces a symlink rather than following its directory.
        mv = self.commands / "mv"
        mv.write_text("#!/usr/bin/env python3\nimport os, sys\nos.replace(sys.argv[-2], sys.argv[-1])\n")
        mv.chmod(0o755)
        rustup = self.cache / "cargo/bin/rustup"
        rustup.parent.mkdir(parents=True)
        rustup.symlink_to(stub)
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.bind(str(self.root / "helper.sock"))
        self.addCleanup(self.sock.close)
        script = GUEST_SCRIPT.read_text()
        for original, replacement in [
            ("root=/var/lib/firecrab/dev", "root=" + shlex.quote(str(self.cache))),
            ("bin_root=/usr/local/lib/firecrab-dev", "bin_root=" + shlex.quote(str(self.binaries))),
            ("unit_root=/etc/systemd/system", "unit_root=" + shlex.quote(str(self.units))),
            ("os_root=/", "os_root=" + shlex.quote(str(self.root))),
            ("/run/firecrab/net-helper.sock", str(self.root / "helper.sock")),
        ]:
            script = script.replace(original, replacement)
        self.script = self.root / "guest.sh"
        self.script.write_text(script)
        source = self.root / "local/firecrab-api/src/main.rs"
        source.parent.mkdir(parents=True)
        source.write_text("edited local source")
        tools = self.root / "local/scripts/firecracker-menual"
        tools.mkdir(parents=True)
        for tool in ["extract-vmlinux", "extract-arm64-image"]:
            (tools / tool).write_text("#!/bin/sh\necho runtime-tool\n")
        self.archive = self.cache / "incoming-test.tar"
        with tarfile.open(self.archive, "w") as archive:
            archive.add(source, arcname="firecrab-api/src/main.rs")
            archive.add(tools, arcname="scripts/firecracker-menual")

    def run_guest(self, profile="debug", shell="bash", **flags):
        environment = os.environ.copy()
        environment.update(TEST_ROOT=str(self.root), PATH=str(self.commands) + ":" + environment["PATH"])
        environment.update({key: str(value) for key, value in flags.items()})
        args = [shell, str(self.script), profile]
        if profile not in ["start", "stop", "restart", "check"]:
            args.extend(["1.97.1", self.archive.name])
        return subprocess.run(args,
                              env=environment, text=True, capture_output=True, timeout=10)

    def override(self, unit="firecrab-api"):
        return self.units / (unit + ".service.d/90-firecrab-dev.conf")

    def test_read_only_os_rejects_start_restart_and_build_even_with_healthy_api(self):
        self.mounts.write_text("/dev/os / ext4 ro,relatime 0 0\n")
        for action in ["check", "start", "restart", "debug", "restore"]:
            with self.subTest(action=action):
                result = self.run_guest(action)
                self.assertEqual(result.returncode, 74, result.stderr)
                self.assertIn("is not writable", result.stderr)
                self.assertNotIn("[PASS]", result.stdout)
                self.assertFalse((self.root / "events").exists())
                self.assertFalse((self.root / "rustup-events").exists())

    def test_read_only_data_mount_is_checked_separately_from_os(self):
        self.mounts.write_text(f"/dev/os / ext4 rw 0 0\n/dev/data {self.cache} ext4 ro 0 0\n")
        self.cache.mkdir(exist_ok=True)
        result = self.run_guest()
        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertIn(str(self.cache), result.stderr)
        self.assertFalse((self.root / "events").exists())

    def test_read_only_overmount_takes_precedence_over_the_original_mount(self):
        self.mounts.write_text("/dev/os / ext4 ro 0 0\n/dev/os / ext4 rw 0 0\n")
        result = self.run_guest("start")
        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertNotIn("[PASS]", result.stdout)

    def test_stop_remains_available_on_read_only_os(self):
        self.mounts.write_text("/dev/os / ext4 ro 0 0\n")
        result = self.run_guest("stop")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("[PASS]", result.stdout)

    def test_disk_failure_during_build_preserves_services_and_recovery_files(self):
        result = self.run_guest(DISK_FAIL_AFTER_BUILD=1)
        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertIn("firecrab service repair", result.stderr)
        self.assertIn("[PRESERVE]", result.stderr)
        self.assertTrue(self.archive.exists())
        self.assertFalse((self.root / "events").exists())
        self.assertFalse(self.override().exists())

    def test_unreadable_system_executable_is_reported_as_os_failure(self):
        chmod = self.commands / "chmod"
        chmod.unlink()
        chmod.write_text("#!/bin/sh\necho 'Input/output error' >&2\nexit 126\n")
        chmod.chmod(0o755)
        result = self.run_guest("start")
        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertIn("OS cannot execute chmod", result.stderr)
        self.assertFalse((self.root / "events").exists())

    def test_legacy_service_uses_the_new_helper_executable_and_restore_removes_its_override(self):
        result = self.run_guest(LEGACY_HELPER=1)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        legacy = self.override("firecrab-net-helper")
        self.assertIn("/bin/firecrab-helper", legacy.read_text())
        self.assertFalse(self.override("firecrab-helper").exists())
        restored = self.run_guest(profile="restore", LEGACY_HELPER=1)
        self.assertEqual(restored.returncode, 0, restored.stderr)
        self.assertFalse(legacy.exists())

    def test_alias_name_development_override_is_removed_when_deploying_or_restoring(self):
        legacy = self.override("firecrab-net-helper")
        legacy.parent.mkdir(parents=True)
        legacy.write_text("old alias override")
        result = self.run_guest()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(legacy.exists())
        legacy.write_text("old alias override")
        restored = self.run_guest(profile="restore")
        self.assertEqual(restored.returncode, 0, restored.stderr)
        self.assertFalse(legacy.exists())
        self.assertFalse(self.override("firecrab-helper").exists())

    def test_cached_toolchain_does_not_require_network_refresh(self):
        result = self.run_guest(TOOLCHAIN_INSTALL_FAIL=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "rustup-events").read_text()
        self.assertIn("run 1.97.1 rustc --version", calls)
        self.assertNotIn("toolchain install", calls)

    def test_missing_toolchain_is_installed_before_build(self):
        result = self.run_guest(TOOLCHAIN_MISSING=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "rustup-events").read_text()
        self.assertLess(calls.index("toolchain install"), calls.index("cargo build"))

    def test_toolchain_download_failure_preserves_running_services(self):
        result = self.run_guest(TOOLCHAIN_MISSING=1, TOOLCHAIN_INSTALL_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "events").exists())
        self.assertFalse(self.override().exists())

    def test_debug_and_release_deploy_edited_source(self):
        for profile in ["debug", "release"]:
            with self.subTest(profile=profile):
                # Each upload is consumed by its build.
                if not self.archive.exists():
                    source = self.root / "local/firecrab-api/src/main.rs"
                    with tarfile.open(self.archive, "w") as archive:
                        archive.add(source, arcname="firecrab-api/src/main.rs")
                        archive.add(self.root / "local/scripts/firecracker-menual", arcname="scripts/firecracker-menual")
                result = self.run_guest(profile)
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                self.assertIn("ExecStart=" + str(self.binaries) + "/bin/firecrab-api", self.override().read_text())
                self.assertIn("built-from-source", (self.binaries / "bin/firecrab-helper").read_text())
                self.assertIn("runtime-tool", (self.binaries / "bin/extract-arm64-image").read_text())
                self.assertFalse(self.archive.exists())

    def test_debug_deploy_works_with_system_bash(self):
        result = self.run_guest(shell="/bin/bash")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertTrue(self.override().is_file())
        self.assertIn("built-from-source", (self.binaries / "bin/firecrab-api").read_text())

    def test_build_failure_does_not_touch_services_or_previous_binaries(self):
        result = self.run_guest(BUILD_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "events").exists())
        self.assertFalse(self.override().exists())
        self.assertFalse((self.binaries / "bin").exists())

    def test_windows_x86_64_checkout_deploys_debug_and_release_with_crlf_tools(self):
        for profile in ["debug", "release"]:
            with self.subTest(profile=profile):
                tools = self.root / "local/scripts/firecracker-menual"
                named_tool = tools / "runtime tool 한글.sh"
                named_tool.write_text("runtime tool")
                named_tool.chmod(0o751)
                for tool in tools.iterdir():
                    tool.write_bytes(b"#!/bin/sh\r\necho runtime-tool\r\n")
                fragment = self.root / "local/firecrab-api/src/oci/guest/install-packages.sh"
                fragment.parent.mkdir(parents=True, exist_ok=True)
                fragment.write_bytes(b"echo embedded-guest-script\r\nprintf 'keep\rinside'\r\n")
                with tarfile.open(self.archive, "w") as archive:
                    archive.add(self.root / "local/firecrab-api/src/main.rs", arcname="firecrab-api/src/main.rs")
                    archive.add(fragment, arcname="firecrab-api/src/oci/guest/install-packages.sh")
                    archive.add(tools, arcname="scripts/firecracker-menual")
                result = self.run_guest(profile, GUEST_ARCH="x86_64")
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                self.assertIn("built-from-source", (self.binaries / "bin/firecrab-api").read_text())
                self.assertNotIn(b"\r", (self.binaries / "bin/extract-vmlinux").read_bytes())
                self.assertEqual((self.root / "compiled-fragment").read_bytes(),
                                 b"echo embedded-guest-script\nprintf 'keep\rinside'\n")
                staged_tool = self.cache / "source/scripts/firecracker-menual" / named_tool.name
                self.assertEqual(staged_tool.read_bytes(), b"#!/bin/sh\necho runtime-tool\n")
                self.assertEqual(staged_tool.stat().st_mode & 0o777, 0o751)

    def test_unsupported_architecture_fails_before_mutating_services(self):
        result = self.run_guest(GUEST_ARCH="riscv64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Linux ARM64 or x86_64 guest required", result.stderr)
        self.assertFalse((self.root / "events").exists())

    def test_failed_first_deployment_restores_release_services(self):
        result = self.run_guest(START_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("[ROLLBACK]", result.stderr)
        self.assertFalse(self.override().exists())
        self.assertFalse((self.binaries / "bin").exists())
        self.assertTrue((self.root / "events").read_text().endswith("start firecrab-api\n"))

    def test_failed_update_restores_previous_development_executables(self):
        previous = self.binaries / "previous"
        previous.mkdir(parents=True)
        (self.binaries / "bin").symlink_to(previous)
        self.override().parent.mkdir(parents=True)
        self.override().write_text("previous override\n")
        result = self.run_guest(HEALTH_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.binaries / "bin").resolve(), previous.resolve())
        self.assertEqual(self.override().read_text(), "previous override\n")

    def test_restore_does_not_build_and_preserves_other_overrides(self):
        self.override().parent.mkdir(parents=True)
        self.override().write_text("development override\n")
        custom = self.override().parent / "custom.conf"
        custom.write_text("custom settings\n")
        result = self.run_guest("restore", BUILD_FAIL=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.override().exists())
        self.assertEqual(custom.read_text(), "custom settings\n")

    def test_restore_after_source_deployment_removes_development_pointer(self):
        result = self.run_guest()
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_guest("restore", BUILD_FAIL=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.override().exists())
        self.assertFalse((self.binaries / "bin").exists())

    def test_concurrent_build_is_rejected_before_mutating_services(self):
        result = self.run_guest(LOCK_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("in progress", result.stderr)
        self.assertFalse((self.root / "events").exists())

    def test_controls_preserve_deployment_and_do_not_build(self):
        for legacy in [0, 1]:
            helper = "firecrab-net-helper" if legacy else "firecrab-helper"
            override = self.override(helper)
            override.parent.mkdir(parents=True, exist_ok=True)
            override.write_text("development helper override\n")
            self.override().parent.mkdir(parents=True, exist_ok=True)
            self.override().write_text("development API override\n")
            previous = self.binaries / "previous"
            previous.mkdir(parents=True, exist_ok=True)
            pointer = self.binaries / "bin"
            if not pointer.exists():
                pointer.symlink_to(previous)
            for action, expected in [
                ("start", [f"start {helper}", "start firecrab-api"]),
                ("stop", ["stop firecrab-api", f"stop {helper}"]),
                ("restart", ["stop firecrab-api", f"stop {helper}", f"start {helper}", "start firecrab-api"]),
            ]:
                with self.subTest(legacy=legacy, action=action):
                    events = self.root / "events"
                    events.unlink(missing_ok=True)
                    result = self.run_guest(action, LEGACY_HELPER=legacy, BUILD_FAIL=1)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    changes = [line for line in events.read_text().splitlines()
                               if line.startswith(("start ", "stop "))]
                    self.assertEqual(changes, expected)
                    self.assertIn("[PASS]", result.stdout)
                    self.assertEqual(override.read_text(), "development helper override\n")
                    self.assertEqual(self.override().read_text(), "development API override\n")
                    self.assertEqual(pointer.resolve(), previous.resolve())
                    self.assertFalse((self.root / "rustup-events").exists())
                    self.assertTrue(self.archive.exists())

    def test_controls_share_the_deployment_lock(self):
        for action in ["start", "stop", "restart"]:
            with self.subTest(action=action):
                result = self.run_guest(action, LOCK_FAIL=1)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("in progress", result.stderr)
                self.assertFalse((self.root / "events").exists())
                self.assertTrue(self.archive.exists())

    def test_failed_stop_does_not_stop_the_helper_or_start_services(self):
        for action in ["stop", "restart"]:
            with self.subTest(action=action):
                (self.root / "events").unlink(missing_ok=True)
                result = self.run_guest(action, STOP_FAIL=1)
                self.assertEqual(result.returncode, 44)
                self.assertEqual((self.root / "events").read_text(), "stop firecrab-api\n")
                self.assertNotIn("[PASS]", result.stdout)

    def test_failed_helper_start_does_not_start_the_api(self):
        result = self.run_guest("start", HELPER_START_FAIL=1)
        self.assertEqual(result.returncode, 45)
        self.assertEqual((self.root / "events").read_text(), "start firecrab-helper\n")
        self.assertNotIn("[PASS]", result.stdout)

    def test_start_and_restart_report_an_unhealthy_api(self):
        for action in ["start", "restart"]:
            with self.subTest(action=action):
                result = self.run_guest(action, HEALTH_FAIL=1)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("guest services did not become ready", result.stderr)
                self.assertNotIn("[PASS]", result.stdout)
                self.assertFalse(self.override().exists())


if __name__ == "__main__":
    unittest.main()
