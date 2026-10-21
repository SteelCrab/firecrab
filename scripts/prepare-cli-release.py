#!/usr/bin/env python3
"""Pin native CLI packages to the host bundles built by the same release run."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tomllib


def prepare(manifest: Path, host_dir: Path, installer: Path, tag: str) -> None:
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError("release tag must be vMAJOR.MINOR.PATCH")
    base = f"https://github.com/SteelCrab/firecrab/releases/download/{tag}"
    updates = {"firecrab": {"version": tag}}
    artifacts = [("installer", installer, f"install-firecrab-{tag}.sh", "install.sh")]
    for arch in ("aarch64", "x86_64"):
        name = f"firecrab-host-{arch}-gnu.tar.gz"
        artifacts.append((arch, host_dir / name, name, name))
    for section, path, filename, asset in artifacts:
        if not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"missing or empty release artifact: {path}")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        updates[f"firecrab.{section}"] = {
            "filename": filename, "url": f"{base}/{asset}", "algorithm": "sha256",
            "digest": digest,
        }
    text = manifest.read_text(encoding="utf-8")
    section = ""
    found = set()
    lines = []
    for line in text.splitlines():
        heading = re.fullmatch(r"\[([^]]+)\]", line.strip())
        if heading:
            section = heading[1]
        entry = re.match(r"^(\w+)\s*=", line)
        if entry and entry[1] in updates.get(section, {}):
            key = entry[1]
            line = f"{key} = {json.dumps(updates[section][key])}"
            found.add((section, key))
        lines.append(line)
    expected = {(section, key) for section, values in updates.items() for key in values}
    if found != expected:
        raise ValueError(f"manifest is missing release fields: {expected-found}")
    result = "\n".join(lines) + "\n"
    parsed = tomllib.loads(result)
    if parsed["firecrab"]["version"] != tag:
        raise ValueError("manifest version was not updated")
    manifest.write_text(result, encoding="utf-8", newline="\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--host-dir", type=Path, required=True)
    parser.add_argument("--installer", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=Path("packaging/micromanager.toml"))
    args = parser.parse_args()
    prepare(args.manifest, args.host_dir, args.installer, args.tag)


if __name__ == "__main__":
    main()
