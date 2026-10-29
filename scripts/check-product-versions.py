#!/usr/bin/env python3
"""Fail a release when Firecrab package versions or install defaults disagree."""

import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def main():
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = workspace["workspace"]["package"]["version"]
    problems = []

    def expect(path, actual):
        if actual != version:
            problems.append(f"{path}: {actual!r}, expected {version}")

    for path in ["Cargo.lock", "firecrab-frontend/Cargo.lock"]:
        lock = tomllib.loads((ROOT / path).read_text())
        for package in lock["package"]:
            if package["name"].startswith("firecrab-"):
                expect(f"{path}: {package['name']}", package["version"])
    frontend = tomllib.loads((ROOT / "firecrab-frontend/Cargo.toml").read_text())
    expect("firecrab-frontend/Cargo.toml", frontend["package"]["version"])
    for directory in ["firecrab-frontend", "firecrab-e2e"]:
        package = json.loads((ROOT / directory / "package.json").read_text())
        lock = json.loads((ROOT / directory / "package-lock.json").read_text())
        expect(f"{directory}/package.json", package["version"])
        expect(f"{directory}/package-lock.json", lock["version"])
        expect(f"{directory}/package-lock.json root", lock["packages"][""]["version"])

    for path in ["packaging/micromanager.toml", "packaging/micromanager-dev.toml"]:
        file = ROOT / path
        if file.exists() and "firecrab" in tomllib.loads(file.read_text()):
            problems.append(f"{path}: fixed Firecrab release inputs must not be bundled")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    print(f"Firecrab product versions agree on {version}; managed installs have no fixed Firecrab release.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
