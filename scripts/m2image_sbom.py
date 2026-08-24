#!/usr/bin/env python3
"""Generate a deterministic SPDX 2.3 M2Image SBOM from a built guest package database.

The builders can feed this script the package-manager state that exists in the
staged root filesystem immediately before it is converted to ext4.  Keeping the
parser separate from the privileged builders makes the release artifact easy to
test and gives CI one normalized contract for Alpine, Ubuntu, and Rocky.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from urllib.parse import quote

RPM_KEY_METADATA_DISPOSITION = "rpm-key-metadata"


def parse_alpine(text: str) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    for paragraph in re.split(r"\n\s*\n", text.strip()):
        fields: dict[str, str] = {}
        for line in paragraph.splitlines():
            if len(line) >= 2 and line[1] == ":":
                fields[line[0]] = line[2:]
        if fields.get("P") and fields.get("V"):
            packages.append(
                {
                    "name": fields["P"],
                    "version": fields["V"],
                    "arch": fields.get("A", "unknown"),
                    "license": fields.get("L", ""),
                    "source": fields.get("o") or fields["P"],
                    "source_version": fields["V"],
                    "source_commit": fields.get("c", ""),
                }
            )
    return packages


def parse_dpkg(text: str) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    for paragraph in re.split(r"\n\s*\n", text.strip()):
        fields: dict[str, str] = {}
        key: str | None = None
        for line in paragraph.splitlines():
            if line.startswith((" ", "\t")):
                if key:
                    fields[key] = f"{fields[key]} {line.strip()}".strip()
                continue
            if ":" not in line:
                continue
            key, value = line.split(":", 1)
            fields[key] = value.strip()
        if fields.get("Status") != "install ok installed":
            continue
        if not fields.get("Package") or not fields.get("Version"):
            continue
        source_field = fields.get("Source", "")
        source = fields["Package"]
        source_version = fields["Version"]
        if source_field:
            match = re.fullmatch(r"([^\s(]+)(?:\s+\(([^)]+)\))?", source_field)
            if not match:
                raise ValueError(f"invalid dpkg Source field: {source_field!r}")
            source = match.group(1)
            source_version = match.group(2) or source_version
        packages.append(
            {
                "name": fields["Package"],
                "version": fields["Version"],
                "arch": fields.get("Architecture", "unknown"),
                "license": "",
                "source": source,
                "source_version": source_version,
                "source_commit": "",
            }
        )
    return packages


def parse_rpm_tsv(text: str) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) != 5:
            raise ValueError(f"rpm TSV line {lineno}: expected 5 fields, got {len(parts)}")
        name, version, arch, license_text, source = parts
        if not name or not version:
            raise ValueError(f"rpm TSV line {lineno}: package name/version may not be empty")
        if source == "(none)" or not source:
            if name != "gpg-pubkey":
                raise ValueError(
                    f"rpm TSV line {lineno}: {name}@{version} has no SOURCERPM"
                )
            packages.append(
                {
                    "name": name,
                    "version": version,
                    "arch": arch or "unknown",
                    "license": license_text,
                    "source": "",
                    "source_version": "",
                    "source_commit": "",
                    "source_disposition": RPM_KEY_METADATA_DISPOSITION,
                }
            )
            continue
        packages.append(
            {
                "name": name,
                "version": version,
                "arch": arch or "unknown",
                "license": license_text,
                "source": source,
                "source_version": version,
                "source_commit": "",
            }
        )
    return packages


PARSERS = {
    "alpine": parse_alpine,
    "dpkg": parse_dpkg,
    "rpm-tsv": parse_rpm_tsv,
}


def stable_package_id(pkg: dict[str, str]) -> str:
    seed = "\0".join((pkg["name"], pkg["version"], pkg["arch"]))
    digest = hashlib.sha256(seed.encode()).hexdigest()[:16]
    clean = re.sub(r"[^A-Za-z0-9.-]+", "-", pkg["name"]).strip("-") or "package"
    return f"SPDXRef-Package-{clean}-{digest}"


def purl_for(distribution: str, pkg: dict[str, str]) -> str:
    distro = distribution.lower()
    if distro == "alpine":
        kind, namespace = "apk", "alpine"
    elif distro == "ubuntu":
        kind, namespace = "deb", "ubuntu"
    elif distro == "rocky":
        kind, namespace = "rpm", "rocky-linux"
    else:
        kind, namespace = "generic", distro
    name = quote(pkg["name"], safe="._+-")
    version = quote(pkg["version"], safe="._+:-~")
    arch = quote(pkg["arch"], safe="._+-")
    return f"pkg:{kind}/{namespace}/{name}@{version}?arch={arch}"


def created_timestamp() -> str:
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if epoch is not None:
        when = dt.datetime.fromtimestamp(int(epoch), tz=dt.timezone.utc)
    else:
        when = dt.datetime.now(dt.timezone.utc)
    return when.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def kernel_packages(distribution: str, packages: list[dict[str, str]]) -> list[dict[str, str]]:
    distro = distribution.lower()
    result = []
    for pkg in packages:
        name = pkg["name"]
        if distro == "alpine" and name == "linux-virt":
            result.append(pkg)
        elif distro == "ubuntu" and name.startswith("linux-image-"):
            result.append(pkg)
        elif distro == "rocky" and name in {"kernel", "kernel-core", "kernel-modules", "kernel-modules-core"}:
            result.append(pkg)
    return result


def make_spdx(
    *,
    distribution: str,
    image_alias: str,
    image_version: str,
    architecture: str,
    packages: list[dict[str, str]],
) -> dict:
    normalized = sorted(packages, key=lambda p: (p["name"], p["version"], p["arch"]))
    if not normalized:
        raise ValueError("package database contained no installed packages")
    if len({(p["name"], p["version"], p["arch"]) for p in normalized}) != len(normalized):
        raise ValueError("package database contains duplicate package records")

    fingerprint = hashlib.sha256(
        json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    image_id = "SPDXRef-M2Image"
    doc: dict = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"FireCrab M2Image {image_alias} {architecture}",
        "documentNamespace": (
            "https://firecrab.dev/spdx/m2image/"
            f"{quote(image_alias, safe='._+-')}/{quote(architecture, safe='._+-')}/{fingerprint}"
        ),
        "creationInfo": {
            "created": created_timestamp(),
            "creators": ["Tool: firecrab scripts/m2image_sbom.py"],
        },
        "packages": [
            {
                "name": image_alias,
                "SPDXID": image_id,
                "versionInfo": image_version,
                "downloadLocation": "NOASSERTION",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": "NOASSERTION",
                "copyrightText": "NOASSERTION",
                "comment": f"distribution={distribution}; architecture={architecture}",
            }
        ],
        "relationships": [
            {
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": image_id,
            }
        ],
        "annotations": [],
    }

    for pkg in normalized:
        spdx_id = stable_package_id(pkg)
        entry = {
            "name": pkg["name"],
            "SPDXID": spdx_id,
            "versionInfo": pkg["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": purl_for(distribution, pkg),
                }
            ],
        }
        comments = []
        if pkg.get("license"):
            comments.append(f"package-manager-license={pkg['license']}")
        if pkg.get("source"):
            comments.append(f"source-package={pkg['source']}")
        if pkg.get("source_version"):
            comments.append(f"source-version={pkg['source_version']}")
        if pkg.get("source_commit"):
            comments.append(f"source-commit={pkg['source_commit']}")
        if pkg.get("source_disposition"):
            comments.append(f"source-disposition={pkg['source_disposition']}")
        if comments:
            entry["comment"] = "; ".join(comments)
        doc["packages"].append(entry)
        doc["relationships"].append(
            {
                "spdxElementId": image_id,
                "relationshipType": "CONTAINS",
                "relatedSpdxElement": spdx_id,
            }
        )

    kernels = kernel_packages(distribution, normalized)
    if kernels:
        doc["annotations"].append(
            {
                "annotationDate": doc["creationInfo"]["created"],
                "annotationType": "OTHER",
                "annotator": "Tool: firecrab scripts/m2image_sbom.py",
                "comment": "kernel-packages="
                + ",".join(f"{p['name']}@{p['version']}" for p in kernels),
            }
        )
    if not doc["annotations"]:
        del doc["annotations"]
    return doc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--format", choices=sorted(PARSERS), required=True)
    parser.add_argument("--distribution", required=True)
    parser.add_argument("--image-alias", required=True)
    parser.add_argument("--image-version", required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--package-db", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)

    try:
        packages = PARSERS[args.format](args.package_db.read_text(encoding="utf-8"))
        document = make_spdx(
            distribution=args.distribution,
            image_alias=args.image_alias,
            image_version=args.image_version,
            architecture=args.architecture,
            packages=packages,
        )
    except (OSError, ValueError) as exc:
        print(f"m2image SBOM: {exc}", file=sys.stderr)
        return 2

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"m2image SBOM: {len(packages)} packages -> {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
