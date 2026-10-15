#!/usr/bin/env python3
"""Run the macOS OS-disk repair helper against an isolated staged image."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import time
from datetime import datetime, timezone


HELPER_TIMEOUT = 120
HELPER_STOP_TIMEOUT = 10
BASE_RESULT_KEYS = {"check_exit", "repair_exit", "verify_exit", "changed"}
RESULT_KEYS = BASE_RESULT_KEYS | {"restored_files"}
REFERENCE_IMAGE_NAME = "debian-system.reference.raw"
DEFAULT_MANAGED_SUFFIX = Path("Library/Application Support/Firecrab/micromanager")
FIXTURE_MARKER = ".firecrab-os-repair-fixture"
FIXTURE_MARKER_CONTENT = "isolated OS repair fixture\n"
PROTECTED_FILES = {
    "fixture_marker": (FIXTURE_MARKER, True),
    "original_os_disk": ("system/debian-system.raw", True),
    "os_kernel": ("system/Image", True),
    "os_initrd": ("system/initrd.img", True),
    "data_disk": ("data/firecrab-data.raw", True),
    "ssh_private_key": ("runtime/manager_ed25519", True),
    "ssh_public_key": ("runtime/manager_ed25519.pub", False),
    "ssh_known_hosts": ("runtime/known_hosts", False),
}


class QAError(Exception):
    """A safety gate, helper contract, or preservation check failed."""


def require(condition, detail):
    if not condition:
        raise QAError(detail)


def absolute_path(value, label):
    path = Path(value).expanduser()
    require(path.is_absolute(), f"{label} must be an absolute path: {path}")
    return Path(os.path.abspath(path))


def reject_symlink_components(path, allow_missing=False):
    """Reject every existing symlink in a path, including ancestor links."""
    path = Path(os.path.abspath(path))
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            mode = current.lstat().st_mode
        except FileNotFoundError:
            if allow_missing:
                continue
            raise QAError(f"path component does not exist: {current}") from None
        except OSError as error:
            raise QAError(f"could not inspect path component {current}: {error}") from error
        require(not stat.S_ISLNK(mode), f"symlinked path is not allowed: {current}")


def path_overlaps(left, right):
    return left == right or left in right.parents or right in left.parents


def actual_user_homes():
    homes = []
    try:
        homes.append(Path(pwd.getpwuid(os.getuid()).pw_dir))
    except (KeyError, OSError):
        pass
    homes.append(Path.home())
    unique = []
    for home in homes:
        normalized = Path(os.path.abspath(home)).resolve(strict=False)
        if normalized not in unique:
            unique.append(normalized)
    return unique


def normalized_existing(path, label):
    reject_symlink_components(path)
    try:
        return path.resolve(strict=True)
    except OSError as error:
        raise QAError(f"could not resolve {label} {path}: {error}") from error


def protected_homes(user_home=None, environ=None):
    environ = os.environ if environ is None else environ
    homes = [home / DEFAULT_MANAGED_SUFFIX for home in (
        actual_user_homes() if user_home is None else [Path(user_home)])]
    configured = environ.get("FIRECRAB_MICROMANAGER_HOME")
    if configured:
        configured_path = Path(configured).expanduser()
        if not configured_path.is_absolute():
            configured_path = Path.cwd() / configured_path
        homes.append(configured_path)
    normalized = []
    for home in homes:
        candidate = Path(os.path.abspath(home))
        try:
            candidate = candidate.resolve(strict=False)
        except OSError as error:
            raise QAError(f"could not resolve protected managed home {home}: {error}") from error
        if candidate not in normalized:
            normalized.append(candidate)
    return normalized


def validate_fixture_home(path, user_home=None, environ=None):
    path = absolute_path(path, "--fixture-home")
    fixture = normalized_existing(path, "fixture home")
    require(fixture.is_dir(), f"fixture home is not a directory: {fixture}")
    for protected in protected_homes(user_home, environ):
        require(not path_overlaps(fixture, protected),
                f"fixture home overlaps a managed installation home: {protected}")
    return fixture


def require_fixture_marker(home):
    marker = home / FIXTURE_MARKER
    required_file(marker)
    try:
        contents = marker.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise QAError(f"could not read fixture marker {marker}: {error}") from error
    require(contents == FIXTURE_MARKER_CONTENT,
            f"fixture marker has unexpected content: {marker}")


def validate_helper(path):
    path = absolute_path(path, "--helper")
    helper = normalized_existing(path, "helper")
    require(helper.is_file(), f"helper is not a regular file: {helper}")
    require(os.access(helper, os.X_OK), f"helper is not executable: {helper}")
    return helper


def required_file(path):
    reject_symlink_components(path, allow_missing=True)
    try:
        info = path.lstat()
    except OSError as error:
        raise QAError(f"required fixture file is missing: {path}: {error}") from error
    require(stat.S_ISREG(info.st_mode), f"fixture asset is not a regular file: {path}")
    return info


def fixture_assets(home):
    return {
        name: (home / relative, required)
        for name, (relative, required) in PROTECTED_FILES.items()
    }


def select_staged_image(home):
    repairs = home / "runtime/repairs"
    reject_symlink_components(repairs)
    require(repairs.is_dir(), f"staging directory is missing: {repairs}")
    candidates = []
    for entry in repairs.iterdir():
        require(not entry.is_symlink(), f"symlinked repair staging entry is not allowed: {entry}")
        if not entry.is_dir():
            continue
        candidate = entry / "debian-system.repaired.raw"
        if candidate.exists() or candidate.is_symlink():
            candidates.append(candidate)
    require(len(candidates) == 1,
            f"expected exactly one staged OS image under {repairs}; found {len(candidates)}")
    staged = candidates[0]
    info = required_file(staged)
    require(info.st_nlink == 1,
            f"staged OS image has {info.st_nlink} hard links; refusing ambiguous ownership: {staged}")
    return staged.resolve(strict=True)


def assert_reference_isolated(reference, staged, home, user_home=None, environ=None):
    info = required_file(reference)
    require(info.st_nlink == 1,
            f"reference OS image must not have hard links: {reference}")
    protected = [(staged, True)] + list(fixture_assets(home).values())
    for protected_home in protected_homes(user_home, environ):
        protected.extend((protected_home / relative, required)
                         for relative, required in PROTECTED_FILES.values())
    for path, _required in protected:
        try:
            if path.exists() or path.is_symlink():
                require(not path.is_symlink(),
                        f"protected disk/SSH asset is a symlink: {path}")
                other = path.stat()
                require((info.st_dev, info.st_ino) != (other.st_dev, other.st_ino),
                        f"reference OS image aliases a protected disk/SSH asset: {path}")
        except OSError as error:
            raise QAError(f"could not inspect protected asset {path}: {error}") from error


def select_reference_image(staged, home, user_home=None, environ=None):
    candidate = staged.with_name(REFERENCE_IMAGE_NAME)
    if not candidate.exists() and not candidate.is_symlink():
        return None
    reject_symlink_components(candidate)
    info = required_file(candidate)
    require(info.st_nlink == 1,
            f"reference OS image must not have hard links: {candidate}")
    assert_reference_isolated(candidate, staged, home, user_home, environ)
    return candidate.resolve(strict=True)


def identity(info):
    return {
        "device": info.st_dev,
        "inode": info.st_ino,
        "size": info.st_size,
        "links": info.st_nlink,
        "mode": stat.S_IMODE(info.st_mode),
    }


def hash_file(path, info):
    digest = hashlib.sha256()
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise QAError(f"could not open fixture asset {path}: {error}") from error
    try:
        opened = os.fstat(descriptor)
        require(stat.S_ISREG(opened.st_mode), f"fixture asset changed type while opening: {path}")
        require((opened.st_dev, opened.st_ino) == (info.st_dev, info.st_ino),
                f"fixture asset changed while opening: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            for block in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(block)
        finished = os.fstat(descriptor)
        require((finished.st_dev, finished.st_ino, finished.st_size,
                 finished.st_mtime_ns, finished.st_ctime_ns) ==
                (opened.st_dev, opened.st_ino, opened.st_size,
                 opened.st_mtime_ns, opened.st_ctime_ns),
                f"fixture asset changed while hashing: {path}")
    finally:
        os.close(descriptor)
    return digest.hexdigest()


def file_record(path, required=True):
    if not path.exists() and not path.is_symlink():
        require(not required, f"required fixture asset is missing: {path}")
        return None
    info = required_file(path)
    return {"path": str(path), "sha256": hash_file(path, info), "identity": identity(info)}


def capture_preservation(home, staged, reference=None):
    protected = {}
    for name, (path, required) in fixture_assets(home).items():
        try:
            protected[name] = file_record(path, required)
        except (QAError, OSError) as error:
            protected[name] = {"error": str(error)}
    if reference is None:
        protected["reference_os_disk"] = None
    else:
        try:
            protected["reference_os_disk"] = file_record(reference)
        except (QAError, OSError) as error:
            protected["reference_os_disk"] = {"error": str(error)}
    try:
        staged_record = file_record(staged)
    except (QAError, OSError) as error:
        staged_record = {"error": str(error)}
    return {
        "protected": protected,
        "staged_os_disk": staged_record,
    }


def require_snapshot_complete(snapshot):
    for name, (_relative, required) in PROTECTED_FILES.items():
        record = snapshot["protected"][name]
        if required:
            require(isinstance(record, dict),
                    f"could not capture required preservation evidence for {name}: {record}")
        else:
            require(record is None or isinstance(record, dict),
                    f"could not capture optional preservation evidence for {name}: {record}")
        if isinstance(record, dict) and "error" in record:
            raise QAError(f"could not capture preservation evidence for {name}: {record['error']}")
    reference = snapshot["protected"].get("reference_os_disk")
    require(reference is None or
            isinstance(reference, dict) and "error" not in reference,
            f"could not capture reference OS image preservation evidence: {reference}")
    require(isinstance(snapshot["staged_os_disk"], dict)
            and "error" not in snapshot["staged_os_disk"],
            f"could not capture staged OS image evidence: {snapshot['staged_os_disk']}")


def require_protected_preserved(before, after):
    before_stage = before["staged_os_disk"]
    require(isinstance(after["staged_os_disk"], dict)
            and "error" not in after["staged_os_disk"],
            f"staged OS image evidence is unavailable after helper: {after['staged_os_disk']}")
    after_stage = after["staged_os_disk"]
    before_identity = before_stage.get("identity", {}) if isinstance(before_stage, dict) else {}
    after_identity = after_stage.get("identity", {})
    require(before_identity.get("links") == 1 and after_identity.get("links") == 1,
            "staged OS image must remain a single-link regular file")
    require((before_identity.get("device"), before_identity.get("inode")) ==
            (after_identity.get("device"), after_identity.get("inode")),
            "helper replaced the staged OS image instead of repairing it in place")
    require(before["protected"] == after["protected"],
            "helper changed or replaced the original OS disk, data disk, or SSH assets")


def assert_staged_isolated(staged, home, user_home=None, environ=None):
    staged_info = required_file(staged)
    require(staged_info.st_nlink == 1,
            f"staged OS image must not have hard links: {staged}")
    protected = list(fixture_assets(home).values())
    for protected_home in protected_homes(user_home, environ):
        protected.extend((protected_home / relative, required)
                         for relative, required in PROTECTED_FILES.values())
    for path, _required in protected:
        try:
            if path.exists() or path.is_symlink():
                require(not path.is_symlink(), f"protected disk/SSH asset is a symlink: {path}")
                info = path.stat()
                require((staged_info.st_dev, staged_info.st_ino) != (info.st_dev, info.st_ino),
                        f"staged OS image aliases a protected disk/SSH asset: {path}")
        except OSError as error:
            raise QAError(f"could not inspect protected asset {path}: {error}") from error


def lsof_path():
    executable = shutil.which("lsof")
    require(executable is not None, "lsof is required to prove fixture images are not in use")
    return executable


def require_image_idle(path, runner=None, executable=None):
    runner = subprocess.run if runner is None else runner
    executable = executable or lsof_path()
    try:
        result = runner([executable, "-t", str(path)], capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise QAError(f"could not check whether fixture image is active ({path}): {error}") from error
    owners = result.stdout.strip().splitlines()
    require(not owners, f"fixture image is open by process(es) {', '.join(owners)}: {path}")
    require(not result.stderr.strip(),
            f"lsof could not prove fixture image is idle ({path}): {result.stderr.strip()}")
    require(result.returncode in (0, 1),
            f"lsof could not prove fixture image is idle ({path}); exit {result.returncode}: {result.stderr.strip()}")
    return {"path": str(path), "exit_code": result.returncode, "owners": owners}


def validate_helper_result(stdout, helper_exit, expect_restored_files=None):
    require(helper_exit == 0, f"repair helper exited with status {helper_exit}")
    if isinstance(stdout, bytes):
        try:
            stdout = stdout.decode("utf-8")
        except UnicodeDecodeError as error:
            raise QAError(f"helper stdout is not UTF-8 JSON: {error}") from error
    try:
        result = json.loads(stdout)
    except (json.JSONDecodeError, TypeError) as error:
        raise QAError(f"helper stdout must contain one JSON object: {error}") from error
    require(isinstance(result, dict), "helper stdout JSON is not an object")
    fields = set(result)
    require(fields in (BASE_RESULT_KEYS, RESULT_KEYS),
            f"helper JSON fields differ from the contract: {sorted(result)}")
    result.setdefault("restored_files", 0)
    for name in ("check_exit", "verify_exit"):
        value = result[name]
        maximum = 7 if name == "check_exit" else 255
        require(type(value) is int and 0 <= value <= maximum,
                f"{name} must be an integer exit status")
    require(type(result["restored_files"]) is int and result["restored_files"] >= 0,
            "restored_files must be a nonnegative integer")
    repair_exit = result["repair_exit"]
    require(repair_exit is None or
            (type(repair_exit) is int and 0 <= repair_exit <= 3),
            "repair_exit must be null or an integer from 0 through 3")
    require(type(result["changed"]) is bool, "changed must be a JSON boolean")
    require(result["verify_exit"] == 0,
            f"helper verification failed with exit {result['verify_exit']}")
    if result["check_exit"] == 0:
        require(result["repair_exit"] is None,
                "clean filesystem must skip filesystem repair")
    else:
        require(result["repair_exit"] is not None,
                "filesystem repair requires a repair exit")
    expected_changed = result["repair_exit"] is not None or result["restored_files"] > 0
    require(result["changed"] is expected_changed,
            "changed must report filesystem repair or restored files")
    if expect_restored_files is not None:
        require(result["restored_files"] == expect_restored_files,
                f"expected {expect_restored_files} restored files, got {result['restored_files']}")
    return result


def write_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    temporary.chmod(0o600)
    temporary.replace(path)


def as_bytes(output):
    if output is None:
        return b""
    return output if isinstance(output, bytes) else str(output).encode("utf-8", errors="replace")


class QA:
    def __init__(self, helper, fixture_home, results_dir, *, user_home=None, environ=None,
                 expect_restored_files=None):
        self.environ = dict(os.environ if environ is None else environ)
        self.user_home = None if user_home is None else Path(user_home)
        self.fixture_input = absolute_path(fixture_home, "--fixture-home")
        self.helper_input = absolute_path(helper, "--helper")
        self.fixture_home = None
        self.helper = None
        self.staged = None
        self.reference = None
        require(expect_restored_files is None or
                (type(expect_restored_files) is int and expect_restored_files >= 0),
                "expected restored file count must be a nonnegative integer")
        self.expect_restored_files = expect_restored_files
        self.results = absolute_path(results_dir, "--results-dir")
        reject_symlink_components(self.results, allow_missing=True)
        require(not self.results.exists(), f"results directory already exists: {self.results}")
        fixture_target = self.fixture_input.resolve(strict=False)
        for protected in [self.fixture_input, fixture_target,
                          *protected_homes(self.user_home, self.environ)]:
            protected = Path(os.path.abspath(protected)).resolve(strict=False)
            require(not path_overlaps(self.results, protected),
                    f"results directory overlaps a protected home: {protected}")
        self.results.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        reject_symlink_components(self.results.parent)
        self.results.mkdir(mode=0o700, exist_ok=False)
        self.results.chmod(0o700)
        self.log = (self.results / "run.log").open("w", encoding="utf-8", buffering=1)
        self.before = None
        self.after = None
        self.summary = {
            "status": "RUNNING",
            "started_at": datetime.now(timezone.utc).isoformat(),
            "platform": platform.platform(),
            "helper": str(self.helper_input),
            "fixture_home": str(self.fixture_input),
            "results_dir": str(self.results),
            "checks": [],
            "helper_exit": None,
            "preservation": "NOT_RUN",
        }
        if expect_restored_files is not None:
            self.summary["expected_restored_files"] = expect_restored_files

    def log_line(self, message):
        self.log.write(message.rstrip("\n") + "\n")
        self.log.flush()

    def check(self, item, name, operation):
        entry = {"id": item, "name": name, "status": "RUNNING"}
        self.summary["checks"].append(entry)
        try:
            result = operation()
        except BaseException as error:
            entry.update(status="FAILED", detail=str(error) or "interrupted")
            raise
        entry["status"] = "PASS"
        return result

    def preflight(self):
        require(platform.system() == "Darwin", "OS repair E2E requires macOS")
        self.fixture_home = validate_fixture_home(
            self.fixture_input, self.user_home, self.environ)
        require_fixture_marker(self.fixture_home)
        self.helper = validate_helper(self.helper_input)
        signature = subprocess.run(
            ["/usr/bin/codesign", "--verify", "--strict", str(self.helper)],
            capture_output=True, text=True, timeout=15,
        )
        require(signature.returncode == 0,
                f"helper code signature did not verify: {signature.stderr.strip() or signature.stdout.strip()}")
        self.summary["helper_signature"] = "PASS"
        self.staged = select_staged_image(self.fixture_home)
        self.reference = select_reference_image(
            self.staged, self.fixture_home, self.user_home, self.environ)
        assets = fixture_assets(self.fixture_home)
        for name in ("fixture_marker", "original_os_disk", "os_kernel", "os_initrd",
                     "data_disk", "ssh_private_key"):
            required_file(assets[name][0])
        assert_staged_isolated(self.staged, self.fixture_home, self.user_home, self.environ)
        image_paths = [assets[name][0] for name in ("original_os_disk", "data_disk")]
        image_paths.append(self.staged)
        idle_images = [require_image_idle(path) for path in image_paths]
        self.summary.update({
            "helper_resolved": str(self.helper),
            "helper_identity": file_record(self.helper),
            "staged_os_disk": str(self.staged),
            "reference_os_disk": str(self.reference) if self.reference is not None else None,
            "reference_image_check": "PASS" if self.reference is not None else "NOT_PRESENT",
            "active_image_check": "PASS",
            "active_images_before": idle_images,
        })

    def capture(self):
        return capture_preservation(self.fixture_home, self.staged, self.reference)

    def record_snapshot(self, name, snapshot):
        write_json(self.results / name, snapshot)

    def stop_child(self, process):
        try:
            os.killpg(process.pid, signal.SIGINT)
        except (ProcessLookupError, PermissionError, AttributeError):
            try:
                process.send_signal(signal.SIGINT)
            except (ProcessLookupError, PermissionError, AttributeError):
                pass
        try:
            return process.communicate(timeout=HELPER_STOP_TIMEOUT)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError, AttributeError):
                try:
                    process.kill()
                except (ProcessLookupError, PermissionError, AttributeError):
                    pass
            return process.communicate()

    def invoke_helper(self):
        command = [str(self.helper), "repair-os", str(self.staged)]
        if self.reference is not None:
            command.extend(["--reference", str(self.reference)])
        environment = dict(self.environ)
        environment["FIRECRAB_MICROMANAGER_HOME"] = str(self.fixture_home)
        self.log_line("$ " + shlex.join(command))
        self.log_line("FIRECRAB_MICROMANAGER_HOME=" + str(self.fixture_home))
        started = time.monotonic()
        stdout = stderr = b""
        process = None
        try:
            process = subprocess.Popen(
                command,
                cwd=self.fixture_home,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            stdout, stderr = process.communicate(timeout=HELPER_TIMEOUT)
        except subprocess.TimeoutExpired:
            self.summary["helper_timed_out"] = True
            stdout, stderr = self.stop_child(process)
            self.summary["helper_exit"] = process.returncode
            self.write_helper_output(stdout, stderr)
            idle_error = self.record_staged_idle_after()
            detail = f"repair helper exceeded {HELPER_TIMEOUT} seconds"
            if idle_error:
                detail += f"; post-helper image idle check failed: {idle_error}"
            raise QAError(detail)
        except KeyboardInterrupt:
            self.summary["helper_interrupted"] = True
            if process is not None:
                stdout, stderr = self.stop_child(process)
                self.summary["helper_exit"] = process.returncode
            self.write_helper_output(stdout, stderr)
            self.record_staged_idle_after()
            raise
        except OSError as error:
            self.summary["helper_launch_error"] = str(error)
            self.write_helper_output(b"", b"")
            self.record_staged_idle_after()
            raise QAError(f"could not start repair helper: {error}") from error
        finally:
            self.summary["helper_elapsed_seconds"] = round(time.monotonic() - started, 3)
        self.summary["helper_exit"] = process.returncode
        self.write_helper_output(stdout, stderr)
        idle_error = self.record_staged_idle_after()
        require(idle_error is None,
                f"post-helper image idle check failed: {idle_error}")
        self.summary["result"] = validate_helper_result(
            stdout, process.returncode, self.expect_restored_files)
        return self.summary["result"]

    def write_helper_output(self, stdout, stderr):
        (self.results / "helper.stdout").write_bytes(as_bytes(stdout))
        (self.results / "helper.stderr.log").write_bytes(as_bytes(stderr))
        (self.results / "helper.stdout").chmod(0o600)
        (self.results / "helper.stderr.log").chmod(0o600)
        self.log_line(f"helper exit={self.summary.get('helper_exit')}")

    def record_staged_idle_after(self):
        try:
            result = require_image_idle(self.staged)
        except QAError as error:
            detail = str(error) or type(error).__name__
            self.summary["staged_image_idle_after"] = {
                "status": "FAILED", "error": detail,
            }
            return detail
        self.summary["staged_image_idle_after"] = {"status": "PASS", **result}
        return None

    def record_preservation(self):
        if self.before is None:
            return
        try:
            self.after = self.capture()
            self.record_snapshot("preservation-after.json", self.after)
            require_snapshot_complete(self.before)
            require_protected_preserved(self.before, self.after)
            self.summary["preservation"] = "PASS"
            self.summary["staged_os_disk_changed"] = (
                self.before["staged_os_disk"]["sha256"] !=
                self.after["staged_os_disk"]["sha256"])
            entry = {"id": "OSR-P", "name": "original disk/data/SSH preservation", "status": "PASS"}
        except BaseException as error:
            self.summary["preservation"] = "FAILED"
            self.summary["preservation_error"] = str(error) or "interrupted"
            entry = {
                "id": "OSR-P", "name": "original disk/data/SSH preservation",
                "status": "FAILED", "detail": str(error) or "interrupted",
            }
            if self.after is None:
                write_json(self.results / "preservation-after-error.json", {"error": entry["detail"]})
        self.summary["checks"].append(entry)

    def execute(self):
        code = 1
        try:
            self.check("OSR-G", "fixture isolation and active-image gate", self.preflight)
            self.before = self.capture()
            self.record_snapshot("preservation-before.json", self.before)
            self.check("OSR-B", "complete preservation baseline", lambda: require_snapshot_complete(self.before))
            self.check("OSR-H", "repair helper contract", self.invoke_helper)
            code = 0
        except KeyboardInterrupt:
            self.summary["error"] = "interrupted"
            code = 130
        except BaseException as error:
            self.summary["error"] = str(error) or type(error).__name__
            code = 1
        finally:
            self.record_preservation()
            if self.summary["preservation"] == "FAILED":
                code = 1
            self.summary["status"] = "PASS" if code == 0 else "FAILED"
            self.summary["exit_code"] = code
            self.summary["finished_at"] = datetime.now(timezone.utc).isoformat()
            write_json(self.results / "summary.json", self.summary)
            self.log_line(f"QA status={self.summary['status']} exit={code}")
            self.log.close()
        print(f"{self.summary['status']}: OS repair QA; evidence: {self.results}", flush=True)
        return code


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", required=True, help="absolute path to signed macOS helper")
    parser.add_argument("--fixture-home", required=True,
                        help="isolated fixture home; installed/default homes are rejected")
    parser.add_argument("--results-dir", required=True, help="new private evidence directory")
    parser.add_argument("--expect-restored-files", type=int,
                        help="require this exact number of restored configuration files")
    args = parser.parse_args(argv)
    try:
        qa = QA(args.helper, args.fixture_home, args.results_dir,
                expect_restored_files=args.expect_restored_files)
    except QAError as error:
        print(f"FAILED OS repair QA setup: {error}", file=sys.stderr)
        return 2
    return qa.execute()


if __name__ == "__main__":
    raise SystemExit(main())
