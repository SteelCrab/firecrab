#!/usr/bin/env python3
"""Build deterministic per-M2Image license evidence and source-provenance metadata.

This does not try to decide whether a distribution's source-availability terms
satisfy a particular license. It records the exact package/source identity that
the image build observed and packages license/copyright material present in the
built guest, plus FireCrab's canonical GPL-2.0 text. Release publication can use
source-map.json as the mechanical input for corresponding-source handling.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path
from urllib.parse import parse_qs, quote, urlsplit

LEGAL_PREFIXES = (
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "authors",
)
RPM_KEY_METADATA_DISPOSITION = "rpm-key-metadata"


def _read_json(path: Path) -> dict:
    with path.open(encoding="utf-8") as stream:
        return json.load(stream)


def _comment_fields(comment: str | None) -> dict[str, str]:
    fields: dict[str, str] = {}
    for part in (comment or "").split(";"):
        if "=" not in part:
            continue
        key, value = part.split("=", 1)
        key = key.strip()
        value = value.strip()
        if key and value:
            fields[key] = value
    return fields


def _package_architecture(package: dict, image_architecture: str) -> str:
    """Return the binary package architecture already encoded in its SPDX purl.

    Older synthetic fixtures and pre-architecture SBOMs may not carry a purl;
    keep the image architecture as a compatibility fallback for those inputs.
    Real M2Image SBOMs emit ``?arch=...`` from the package database, preserving
    values such as Rocky ``noarch`` instead of rewriting them to the image arch.
    """
    for reference in package.get("externalRefs") or []:
        if reference.get("referenceType") != "purl":
            continue
        locator = reference.get("referenceLocator")
        if not isinstance(locator, str):
            continue
        try:
            values = parse_qs(urlsplit(locator).query).get("arch")
        except ValueError:
            continue
        if values and values[0]:
            return values[0]
    return image_architecture


def _source_resolver(distribution: str, source: str, source_version: str, commit: str) -> dict:
    distro = distribution.lower()
    if distro == "alpine":
        resolver = {
            "type": "alpine-aports",
            "sourcePackage": source,
            "sourceVersion": source_version,
        }
        if commit:
            resolver["repositoryCommit"] = commit
            resolver["metadataUrl"] = (
                "https://gitlab.alpinelinux.org/alpine/aports/-/commit/"
                + quote(commit, safe="")
            )
        return resolver
    if distro == "ubuntu":
        return {
            "type": "ubuntu-source-package",
            "sourcePackage": source,
            "sourceVersion": source_version,
            "metadataUrl": (
                "https://launchpad.net/ubuntu/+source/"
                + quote(source, safe="._+-")
                + "/"
                + quote(source_version, safe="._+:-~")
            ),
        }
    if distro == "rocky":
        return {
            "type": "rocky-source-rpm",
            "sourceArtifact": source,
            "sourceVersion": source_version,
        }
    return {
        "type": "distribution-source-package",
        "sourcePackage": source,
        "sourceVersion": source_version,
    }


def _non_source_disposition(distribution: str, package_name: str, disposition: str) -> str:
    if (
        distribution.lower() == "rocky"
        and package_name == "gpg-pubkey"
        and disposition == RPM_KEY_METADATA_DISPOSITION
    ):
        return disposition
    raise ValueError(
        f"{package_name}: unsupported source-disposition {disposition!r}"
    )


def source_map(spdx: dict) -> dict:
    packages = spdx.get("packages") or []
    if len(packages) < 2:
        raise ValueError("SPDX document contains no installed guest packages")
    image = packages[0]
    image_fields = _comment_fields(image.get("comment"))
    distribution = image_fields.get("distribution", "unknown")
    architecture = image_fields.get("architecture", "unknown")
    records = []
    for package in packages[1:]:
        fields = _comment_fields(package.get("comment"))
        package_name = str(package.get("name") or "<unknown>")
        binary_version = str(package.get("versionInfo") or "unknown")
        package_architecture = _package_architecture(package, architecture)
        disposition = fields.get("source-disposition", "")
        if disposition:
            records.append(
                {
                    "binaryPackage": package.get("name"),
                    "binaryVersion": binary_version,
                    "architecture": package_architecture,
                    "declaredLicense": fields.get("package-manager-license") or None,
                    "sourceDisposition": _non_source_disposition(
                        distribution, package_name, disposition
                    ),
                }
            )
            continue

        source = fields.get("source-package")
        if not source:
            raise ValueError(
                f"{package_name}@{binary_version}: missing source-package identity"
            )
        source_version = fields.get("source-version") or binary_version
        source_commit = fields.get("source-commit", "")
        records.append(
            {
                "binaryPackage": package.get("name"),
                "binaryVersion": binary_version,
                "architecture": package_architecture,
                "declaredLicense": fields.get("package-manager-license") or None,
                "source": _source_resolver(
                    distribution, source, source_version, source_commit
                ),
            }
        )
    records.sort(key=lambda item: (str(item["binaryPackage"]), item["binaryVersion"]))
    return {
        "schemaVersion": 1,
        "image": {
            "alias": image.get("name"),
            "version": image.get("versionInfo"),
            "distribution": distribution,
            "architecture": architecture,
        },
        "packages": records,
    }


def _legal_file(path: Path, legal_root: Path) -> bool:
    rel = path.relative_to(legal_root)
    parts = rel.parts
    if len(parts) >= 3 and parts[:3] == ("usr", "share", "licenses"):
        return True
    if len(parts) >= 3 and parts[:3] == ("usr", "share", "common-licenses"):
        return True
    if len(parts) >= 3 and parts[:3] == ("usr", "share", "spdx"):
        return True
    if len(parts) >= 3 and parts[:3] == ("usr", "share", "doc"):
        name = path.name.lower()
        return name.startswith(LEGAL_PREFIXES)
    return False


def _copy_guest_legal_files(legal_root: Path, destination: Path) -> list[dict]:
    if not legal_root.is_dir():
        return []
    records = []
    for path in sorted(legal_root.rglob("*")):
        if not path.is_file() or path.is_symlink() or not _legal_file(path, legal_root):
            continue
        rel = path.relative_to(legal_root)
        output = destination / "guest" / rel
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, output)
        data = output.read_bytes()
        records.append(
            {
                "guestPath": "/" + rel.as_posix(),
                "bundlePath": output.relative_to(destination.parent).as_posix(),
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
    return records


def build_bundle(*, spdx_path: Path, legal_root: Path, gpl2_text: Path, output_dir: Path) -> dict:
    spdx = _read_json(spdx_path)
    if spdx.get("spdxVersion") != "SPDX-2.3":
        raise ValueError("M2Image SBOM is not SPDX 2.3")
    if not gpl2_text.is_file():
        raise ValueError(f"canonical GPL-2.0 text not found: {gpl2_text}")

    if output_dir.exists():
        shutil.rmtree(output_dir)
    licenses_dir = output_dir / "licenses"
    licenses_dir.mkdir(parents=True)

    source_document = source_map(spdx)
    (output_dir / "source-map.json").write_text(
        json.dumps(source_document, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    records = _copy_guest_legal_files(legal_root, licenses_dir)
    gpl_target = licenses_dir / "GPL-2.0-only.txt"
    shutil.copyfile(gpl2_text, gpl_target)
    gpl_data = gpl_target.read_bytes()
    records.append(
        {
            "guestPath": None,
            "bundlePath": "licenses/GPL-2.0-only.txt",
            "bytes": len(gpl_data),
            "sha256": hashlib.sha256(gpl_data).hexdigest(),
            "provenance": "FireCrab canonical license text",
        }
    )
    records.sort(key=lambda item: item["bundlePath"])
    (licenses_dir / "index.json").write_text(
        json.dumps({"schemaVersion": 1, "files": records}, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )

    package_count = len(source_document["packages"])
    summary = {
        "schemaVersion": 1,
        "image": source_document["image"],
        "packageCount": package_count,
        "guestLegalFileCount": sum(1 for item in records if item.get("guestPath")),
        "licenseFileCount": len(records),
        "sourceMap": "source-map.json",
        "licenseIndex": "licenses/index.json",
    }
    (output_dir / "bundle.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / "README.txt").write_text(
        "FireCrab M2Image compliance bundle\n"
        "=================================\n\n"
        "sbom.spdx.json identifies the installed binary packages. source-map.json\n"
        "records the package-manager source identity or an explicit non-source\n"
        "disposition observed for every binary. licenses/ contains copyright/license\n"
        "material recovered from the built guest plus FireCrab's canonical GPL-2.0\n"
        "text. Source-map metadata is an input to release corresponding-source\n"
        "publication; it is not itself a statement that corresponding-source\n"
        "obligations have been satisfied.\n",
        encoding="utf-8",
    )
    shutil.copyfile(spdx_path, output_dir / "sbom.spdx.json")
    return summary


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sbom", type=Path, required=True)
    parser.add_argument("--legal-root", type=Path, required=True)
    parser.add_argument("--gpl2-text", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        summary = build_bundle(
            spdx_path=args.sbom,
            legal_root=args.legal_root,
            gpl2_text=args.gpl2_text,
            output_dir=args.output_dir,
        )
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"m2image compliance: {exc}", file=sys.stderr)
        return 2
    print(
        "m2image compliance: "
        f"{summary['packageCount']} packages, "
        f"{summary['guestLegalFileCount']} guest legal files -> {args.output_dir}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
