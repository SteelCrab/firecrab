#!/usr/bin/env python3
"""Plan and index corresponding-source publication for an M2Image.

The source map records package-manager source identity. This module turns that
identity into a deterministic publication plan and, after source material has
been fetched, a hash-indexed source bundle. The policy is intentionally
conservative: every installed package must have an explicit source disposition,
not only packages that a partial license classifier happens to recognize.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

ALPINE_COMMIT = re.compile(r"^[0-9a-f]{40}$")
RPM_KEY_METADATA_DISPOSITION = "rpm-key-metadata"


def _read_json(path: Path) -> dict:
    with path.open(encoding="utf-8") as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError(f"expected JSON object: {path}")
    return value


def _source_key(source: dict) -> str:
    payload = json.dumps(source, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode()).hexdigest()[:20]


def _normalized_source(distribution: str, source: dict) -> dict:
    resolver_type = str(source.get("type") or "")
    distro = distribution.lower()

    if distro == "alpine":
        if resolver_type != "alpine-aports":
            raise ValueError(f"Alpine package has unexpected source resolver: {resolver_type!r}")
        package = str(source.get("sourcePackage") or "")
        version = str(source.get("sourceVersion") or "")
        commit = str(source.get("repositoryCommit") or "")
        if not package or not version or not ALPINE_COMMIT.fullmatch(commit):
            raise ValueError(
                "Alpine source identity requires sourcePackage, sourceVersion, and exact 40-hex aports commit"
            )
        return {
            "type": resolver_type,
            "sourcePackage": package,
            "sourceVersion": version,
            "repositoryCommit": commit,
            "repository": "https://gitlab.alpinelinux.org/alpine/aports.git",
            "fetchStrategy": "aports-recipe-and-abuild-distfiles",
        }

    if distro == "ubuntu":
        if resolver_type != "ubuntu-source-package":
            raise ValueError(f"Ubuntu package has unexpected source resolver: {resolver_type!r}")
        package = str(source.get("sourcePackage") or "")
        version = str(source.get("sourceVersion") or "")
        if not package or not version:
            raise ValueError("Ubuntu source identity requires sourcePackage and exact sourceVersion")
        return {
            "type": resolver_type,
            "sourcePackage": package,
            "sourceVersion": version,
            "fetchStrategy": "apt-get-source-download-only",
        }

    if distro == "rocky":
        if resolver_type != "rocky-source-rpm":
            raise ValueError(f"Rocky package has unexpected source resolver: {resolver_type!r}")
        artifact = str(source.get("sourceArtifact") or "")
        version = str(source.get("sourceVersion") or "")
        if not artifact or not version:
            raise ValueError("Rocky source identity requires sourceArtifact and sourceVersion")
        if not artifact.endswith((".src.rpm", ".nosrc.rpm")):
            raise ValueError(f"Rocky source artifact is not an SRPM: {artifact!r}")
        return {
            "type": resolver_type,
            "sourceArtifact": artifact,
            "sourceVersion": version,
            "fetchStrategy": "dnf-download-source-rpm",
        }

    raise ValueError(f"unsupported M2Image distribution for source publication: {distribution!r}")


def _non_source_record(distribution: str, package: dict) -> dict | None:
    disposition = str(package.get("sourceDisposition") or "")
    if not disposition:
        return None
    binary = str(package.get("binaryPackage") or "")
    if (
        distribution.lower() != "rocky"
        or binary != "gpg-pubkey"
        or disposition != RPM_KEY_METADATA_DISPOSITION
    ):
        raise ValueError(
            f"{binary or '<unknown>'}: unsupported non-source disposition {disposition!r}"
        )
    return {
        "name": binary,
        "version": str(package.get("binaryVersion") or ""),
        "architecture": str(package.get("architecture") or ""),
        "declaredLicense": package.get("declaredLicense"),
        "sourceDisposition": disposition,
        "reason": "RPM imported public-key metadata has no SOURCERPM payload",
    }


def publication_plan(source_map: dict) -> dict:
    if source_map.get("schemaVersion") != 1:
        raise ValueError("source map schemaVersion must be 1")
    image = source_map.get("image")
    packages = source_map.get("packages")
    if not isinstance(image, dict) or not isinstance(packages, list) or not packages:
        raise ValueError("source map requires image metadata and installed packages")

    distribution = str(image.get("distribution") or "")
    if not distribution:
        raise ValueError("source map image is missing distribution")

    grouped: dict[str, dict] = {}
    non_source_packages: list[dict] = []
    seen_binaries: set[tuple[str, str, str]] = set()
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("source map package record must be an object")
        binary = str(package.get("binaryPackage") or "")
        version = str(package.get("binaryVersion") or "")
        architecture = str(package.get("architecture") or "")
        if not binary or not version or not architecture:
            raise ValueError("source map package is missing binary name/version/architecture")
        binary_key = (binary, version, architecture)
        if binary_key in seen_binaries:
            raise ValueError(f"duplicate binary package in source map: {binary}@{version}/{architecture}")
        seen_binaries.add(binary_key)

        non_source = _non_source_record(distribution, package)
        if non_source is not None:
            if package.get("source") is not None:
                raise ValueError(f"{binary}@{version}: non-source package must not carry a source resolver")
            non_source_packages.append(non_source)
            continue

        raw_source = package.get("source")
        if not isinstance(raw_source, dict):
            raise ValueError(f"{binary}@{version}: source resolver is missing")
        source = _normalized_source(distribution, raw_source)
        source_id = _source_key(source)
        unit = grouped.setdefault(
            source_id,
            {
                "sourceId": source_id,
                "source": source,
                "binaryPackages": [],
                "bundlePath": f"sources/{source_id}",
            },
        )
        unit["binaryPackages"].append(
            {
                "name": binary,
                "version": version,
                "architecture": architecture,
                "declaredLicense": package.get("declaredLicense"),
            }
        )

    sources = []
    for source_id in sorted(grouped):
        unit = grouped[source_id]
        unit["binaryPackages"].sort(
            key=lambda item: (item["name"], item["version"], item["architecture"])
        )
        sources.append(unit)
    non_source_packages.sort(
        key=lambda item: (item["name"], item["version"], item["architecture"])
    )

    source_backed_count = sum(len(item["binaryPackages"]) for item in sources)
    if source_backed_count + len(non_source_packages) != len(packages):
        raise ValueError("source publication plan does not account for every installed package")

    return {
        "schemaVersion": 1,
        "coveragePolicy": "all-installed-packages",
        "image": image,
        "packageCount": len(packages),
        "sourceBackedPackageCount": source_backed_count,
        "nonSourcePackageCount": len(non_source_packages),
        "sourceCount": len(sources),
        "sources": sources,
        "nonSourcePackages": non_source_packages,
    }


def source_index(plan: dict, source_root: Path) -> dict:
    if plan.get("schemaVersion") != 1 or plan.get("coveragePolicy") != "all-installed-packages":
        raise ValueError("source publication plan is not a supported schema/policy")
    sources = plan.get("sources")
    non_source_packages = plan.get("nonSourcePackages", [])
    if not isinstance(sources, list) or not isinstance(non_source_packages, list):
        raise ValueError("source publication plan has malformed source coverage sections")

    indexed = []
    binary_coverage = 0
    for unit in sources:
        source_id = str(unit.get("sourceId") or "")
        expected = f"sources/{source_id}"
        if unit.get("bundlePath") != expected:
            raise ValueError(f"source unit {source_id}: bundlePath must be {expected}")
        directory = source_root / source_id
        if not directory.is_dir():
            raise ValueError(f"source unit {source_id}: source directory is missing")
        files = []
        for path in sorted(directory.rglob("*")):
            if not path.is_file() or path.is_symlink():
                continue
            data = path.read_bytes()
            files.append(
                {
                    "path": path.relative_to(source_root.parent).as_posix(),
                    "bytes": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                }
            )
        if not files:
            raise ValueError(f"source unit {source_id}: no source files were published")
        binaries = unit.get("binaryPackages") or []
        binary_coverage += len(binaries)
        indexed.append(
            {
                "sourceId": source_id,
                "source": unit.get("source"),
                "binaryPackages": binaries,
                "files": files,
            }
        )

    binary_coverage += len(non_source_packages)
    expected_packages = int(plan.get("packageCount") or 0)
    if binary_coverage != expected_packages:
        raise ValueError(
            f"source bundle accounts for {binary_coverage} binaries but plan requires {expected_packages}"
        )

    return {
        "schemaVersion": 1,
        "coveragePolicy": plan["coveragePolicy"],
        "image": plan.get("image"),
        "packageCount": expected_packages,
        "sourceBackedPackageCount": expected_packages - len(non_source_packages),
        "nonSourcePackageCount": len(non_source_packages),
        "sourceCount": len(indexed),
        "fileCount": sum(len(item["files"]) for item in indexed),
        "sources": indexed,
        "nonSourcePackages": non_source_packages,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    plan_parser = sub.add_parser("plan", help="create a deterministic source-publication plan")
    plan_parser.add_argument("--source-map", type=Path, required=True)
    plan_parser.add_argument("--output", type=Path, required=True)

    index_parser = sub.add_parser("index", help="hash and validate fetched source material")
    index_parser.add_argument("--plan", type=Path, required=True)
    index_parser.add_argument("--source-root", type=Path, required=True)
    index_parser.add_argument("--output", type=Path, required=True)

    args = parser.parse_args(argv)
    try:
        if args.command == "plan":
            document = publication_plan(_read_json(args.source_map))
        else:
            document = source_index(_read_json(args.plan), args.source_root)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"m2image source publication: {exc}", file=sys.stderr)
        return 2

    print(
        "m2image source publication: "
        f"packages={document['packageCount']} sources={document['sourceCount']} "
        f"non_source={document.get('nonSourcePackageCount', 0)} -> {args.output}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
