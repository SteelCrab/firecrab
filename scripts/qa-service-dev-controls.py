#!/usr/bin/env python3
"""Show one isolated development-control test without accessing a management VM."""

import argparse
from datetime import datetime
import importlib.util
from pathlib import Path
import sys
from zoneinfo import ZoneInfo


def run(action):
    contract = Path(__file__).with_name("test-micromanager-dev.py")
    spec = importlib.util.spec_from_file_location("guest_development_contract", contract)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    fixture = module.GuestDevelopmentTest(methodName="runTest")
    try:
        fixture.setUp()
        deployed = fixture.run_guest(shell="/bin/bash")
        assert deployed.returncode == 0, deployed.stdout + deployed.stderr
        previous = (fixture.binaries / "bin").resolve()
        override = fixture.override().read_text()
        events = fixture.root / "events"
        events.unlink()
        (fixture.root / "rustup-events").unlink()
        data = fixture.root / "user-data"
        data.mkdir()
        (data / "disk.raw").write_text("preserved VM data\n")
        custom = fixture.override().parent / "custom.conf"
        custom.write_text("preserved custom settings\n")
        (fixture.cache / ".hidden-cache").write_text("cached build file\n")
        lock_inode = (fixture.cache / "deploy.lock").stat().st_ino

        print("FIRECRAB | ISOLATED DEVELOPMENT CONTROL TEST", flush=True)
        print(datetime.now(ZoneInfo("Asia/Seoul")).strftime("%Y-%m-%d %H:%M:%S KST"), flush=True)
        print("Scope: embedded guest script; stubbed systemctl and curl", flush=True)
        print("No live management VM, installed service, or user data access.", flush=True)
        print(f"\nCommand contract: firecrab service dev {action}", flush=True)
        print(f"$ /bin/bash scripts/micromanager/dev-macos-guest.sh {action}", flush=True)
        result = fixture.run_guest(action, shell="/bin/bash", BUILD_FAIL=1)
        print(result.stdout, end="", flush=True)
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr, flush=True)
        assert result.returncode == 0, f"guest action exited {result.returncode}"

        operations = [line for line in events.read_text().splitlines()
                      if line.startswith(("start ", "stop "))]
        expected = {
            "start": ["start firecrab-helper", "start firecrab-api"],
            "stop": ["stop firecrab-api", "stop firecrab-helper"],
            "restart": ["stop firecrab-api", "stop firecrab-helper",
                        "start firecrab-helper", "start firecrab-api"],
            "clean": ["stop firecrab-api firecrab-helper",
                      "start firecrab-helper", "start firecrab-api"],
        }[action]
        assert operations == expected, operations
        print("\nRecorded service operations:", flush=True)
        for operation in operations:
            print(f"  systemctl {operation}", flush=True)
        print("\n[PASS] API/helper operation order", flush=True)
        assert not (fixture.root / "rustup-events").exists()
        print("[PASS] No compiler or build invoked", flush=True)
        assert (data / "disk.raw").read_text() == "preserved VM data\n"
        assert custom.read_text() == "preserved custom settings\n"
        assert (fixture.root / "local/firecrab-api/src/main.rs").is_file()
        print("[PASS] User data, custom overrides, and host checkout preserved", flush=True)
        if action == "clean":
            assert not fixture.override().exists()
            assert not fixture.binaries.exists()
            assert sorted(path.name for path in fixture.cache.iterdir()) == ["deploy.lock"]
            assert (fixture.cache / "deploy.lock").stat().st_ino == lock_inode
            print("[PASS] Release overrides restored; development binaries removed", flush=True)
            print("[PASS] Source, toolchain, build, and hidden caches removed", flush=True)
            print("[PASS] Deployment lock inode preserved", flush=True)
        else:
            assert (fixture.binaries / "bin").resolve() == previous
            assert fixture.override().read_text() == override
            assert (fixture.cache / ".hidden-cache").is_file()
            print("[PASS] Development executables, overrides, and caches preserved", flush=True)
        print(f"\nRESULT: PASS | {action} | exit code 0", flush=True)
    finally:
        fixture.doCleanups()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["start", "stop", "restart", "clean"])
    run(parser.parse_args().action)
