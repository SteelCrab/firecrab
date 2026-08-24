#!/usr/bin/env python3
"""Validate and query the single source of truth for M2Image releases."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any


ALIAS_RE = re.compile(r"^[a-z0-9]+(?:[.-][a-z0-9]+)*$")
ENV_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
SUPPORTED_ARCHITECTURES = ("x86_64", "aarch64")


class ManifestError(ValueError):
    pass


def fail(message: str) -> None:
    raise ManifestError(message)


def require_string(value: Any, location: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"{location} must be a non-empty string")
    return value


def require_relative_path(value: Any, location: str) -> str:
    value = require_string(value, location)
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        fail(f"{location} must be a safe relative path")
    return value


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        fail(f"cannot read manifest {path}: {error}")
    except json.JSONDecodeError as error:
        fail(f"invalid JSON in {path}: {error}")
    validate_manifest(data)
    return data


def validate_manifest(data: Any) -> None:
    if not isinstance(data, dict):
        fail("manifest root must be an object")
    if data.get("schemaVersion") != 1:
        fail("schemaVersion must be 1")
    registry = data.get("registry")
    if not isinstance(registry, dict):
        fail("registry must be an object")
    require_relative_path(registry.get("catalogKey"), "registry.catalogKey")
    architectures = data.get("architectures")
    if architectures != list(SUPPORTED_ARCHITECTURES):
        fail("architectures must be exactly [\"x86_64\", \"aarch64\"]")
    images = data.get("images")
    if not isinstance(images, list) or not images:
        fail("images must be a non-empty array")

    seen_aliases: set[str] = set()
    seen_keys: set[str] = set()
    for index, image in enumerate(images):
        location = f"images[{index}]"
        if not isinstance(image, dict):
            fail(f"{location} must be an object")
        alias = require_string(image.get("alias"), f"{location}.alias")
        if not ALIAS_RE.fullmatch(alias):
            fail(f"{location}.alias has an invalid format: {alias}")
        if alias in seen_aliases:
            fail(f"duplicate alias: {alias}")
        seen_aliases.add(alias)
        for field in ("distribution", "series", "version", "templateVersion"):
            require_string(image.get(field), f"{location}.{field}")
        for field in ("revision", "minDiskGb"):
            value = image.get(field)
            if not isinstance(value, int) or isinstance(value, bool) or value < 1:
                fail(f"{location}.{field} must be a positive integer")
        expected_template_version = (
            f"{image['distribution']}-{image['version']}-v{image['revision']}"
        )
        if image["templateVersion"] != expected_template_version:
            fail(f"{location}.templateVersion must be {expected_template_version}")

        builder = image.get("builder")
        if not isinstance(builder, dict):
            fail(f"{location}.builder must be an object")
        require_relative_path(builder.get("script"), f"{location}.builder.script")
        requires = builder.get("requires")
        if not isinstance(requires, list) or not all(isinstance(item, str) and item for item in requires):
            fail(f"{location}.builder.requires must be an array of command names")
        environment = builder.get("environment")
        if not isinstance(environment, dict):
            fail(f"{location}.builder.environment must be an object")
        for key, value in environment.items():
            if not ENV_RE.fullmatch(key) or not isinstance(value, str):
                fail(f"{location}.builder.environment contains an invalid entry: {key}")

        artifacts = image.get("artifacts")
        if not isinstance(artifacts, dict) or set(artifacts) != set(SUPPORTED_ARCHITECTURES):
            fail(f"{location}.artifacts must define x86_64 and aarch64 only")
        for architecture in SUPPORTED_ARCHITECTURES:
            artifact = artifacts[architecture]
            artifact_location = f"{location}.artifacts.{architecture}"
            if not isinstance(artifact, dict):
                fail(f"{artifact_location} must be an object")
            kernel = require_relative_path(artifact.get("kernel"), f"{artifact_location}.kernel")
            rootfs = require_relative_path(artifact.get("rootfs"), f"{artifact_location}.rootfs")
            initrd = artifact.get("initrd")
            if initrd is not None:
                require_relative_path(initrd, f"{artifact_location}.initrd")
            require_string(artifact.get("bootArgs"), f"{artifact_location}.bootArgs")
            registry_key = require_relative_path(
                artifact.get("registryKey"), f"{artifact_location}.registryKey"
            )
            if not kernel.startswith("kernel/") or (initrd and not initrd.startswith("kernel/")):
                fail(f"{artifact_location} kernel artifacts must be below kernel/")
            if not rootfs.startswith("rootfs/"):
                fail(f"{artifact_location}.rootfs must be below rootfs/")
            if not registry_key.endswith(f"/{alias}.tar.zst"):
                fail(f"{artifact_location}.registryKey must end with /{alias}.tar.zst")
            immutable_suffix = (
                f"/{image['templateVersion']}/r{image['revision']}/"
                f"{architecture}/{alias}.tar.zst"
            )
            if not registry_key.endswith(immutable_suffix):
                fail(
                    f"{artifact_location}.registryKey must end with "
                    f"{immutable_suffix}"
                )
            if registry_key in seen_keys:
                fail(f"duplicate registry key: {registry_key}")
            seen_keys.add(registry_key)


def find_image(manifest: dict[str, Any], alias: str) -> dict[str, Any]:
    for image in manifest["images"]:
        if image["alias"] == alias:
            return image
    fail(f"unknown alias: {alias}")


def get_field(value: Any, dotted_field: str) -> Any:
    for part in dotted_field.split("."):
        if not isinstance(value, dict) or part not in value:
            fail(f"unknown field: {dotted_field}")
        value = value[part]
    return value


def source_registry_key(manifest: dict[str, Any], alias: str, architecture: str) -> str:
    artifact = find_image(manifest, alias)["artifacts"][architecture]
    package_key = str(artifact["registryKey"])
    suffix = f"/{alias}.tar.zst"
    if not package_key.endswith(suffix):
        fail(f"registry key for {alias}/{architecture} has an unexpected package suffix")
    return package_key[: -len(suffix)] + f"/{alias}.sources.tar.zst"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def published_at(path: Path) -> str:
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", str(int(path.stat().st_mtime))))
    return dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc).isoformat().replace("+00:00", "Z")


def build_catalog(
    manifest: dict[str, Any],
    dist_dir: Path,
    aliases: list[str],
    architectures: list[str],
) -> dict[str, Any]:
    entries: list[dict[str, Any]] = []
    for alias in aliases:
        image = find_image(manifest, alias)
        for architecture in architectures:
            artifact = image["artifacts"][architecture]
            package = dist_dir / architecture / f"{alias}.tar.zst"
            source_package = dist_dir / architecture / f"{alias}.sources.tar.zst"
            if not package.is_file():
                fail(f"package not found: {package}")
            if not source_package.is_file():
                fail(f"source package not found: {source_package}")
            entries.append(
                {
                    "alias": alias,
                    "distribution": image["distribution"],
                    "series": image["series"],
                    "architecture": architecture,
                    "version": image["templateVersion"],
                    "distributionVersion": image["version"],
                    "package": artifact["registryKey"],
                    "sha256": sha256(package),
                    "sizeBytes": package.stat().st_size,
                    "source": source_registry_key(manifest, alias, architecture),
                    "sourceSha256": sha256(source_package),
                    "sourceSizeBytes": source_package.stat().st_size,
                    "minDiskGb": image["minDiskGb"],
                    "publishedAt": published_at(package),
                }
            )
    return {"schemaVersion": 1, "images": entries}


def parser() -> argparse.ArgumentParser:
    repo_dir = Path(__file__).resolve().parent.parent
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument(
        "--manifest",
        type=Path,
        default=repo_dir / "packaging" / "m2images.json",
        help="release manifest path",
    )
    subparsers = result.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate")
    subparsers.add_parser("aliases")
    subparsers.add_parser("architectures")

    field = subparsers.add_parser("field")
    field.add_argument("alias")
    field.add_argument("field")

    environment = subparsers.add_parser("environment")
    environment.add_argument("alias")

    requires = subparsers.add_parser("requires")
    requires.add_argument("alias")

    artifacts = subparsers.add_parser("artifacts")
    artifacts.add_argument("alias")
    artifacts.add_argument("architecture", choices=SUPPORTED_ARCHITECTURES)

    registry_key = subparsers.add_parser("registry-key")
    registry_key.add_argument("alias")
    registry_key.add_argument("architecture", choices=SUPPORTED_ARCHITECTURES)

    source_key = subparsers.add_parser("source-registry-key")
    source_key.add_argument("alias")
    source_key.add_argument("architecture", choices=SUPPORTED_ARCHITECTURES)

    catalog = subparsers.add_parser("catalog")
    catalog.add_argument("--dist-dir", type=Path, required=True)
    catalog.add_argument("--alias", action="append", dest="aliases")
    catalog.add_argument(
        "--architecture",
        action="append",
        dest="architectures",
        choices=SUPPORTED_ARCHITECTURES,
    )
    catalog.add_argument("--output", type=Path)
    return result


def main() -> int:
    args = parser().parse_args()
    manifest = load_manifest(args.manifest)
    if args.command == "validate":
        print(f"valid: {args.manifest}")
    elif args.command == "aliases":
        for image in manifest["images"]:
            print(image["alias"])
    elif args.command == "architectures":
        print("\n".join(manifest["architectures"]))
    elif args.command == "field":
        value = get_field(find_image(manifest, args.alias), args.field)
        if isinstance(value, (dict, list)):
            print(json.dumps(value, separators=(",", ":")))
        else:
            print(value)
    elif args.command == "environment":
        environment = find_image(manifest, args.alias)["builder"]["environment"]
        for key in sorted(environment):
            print(f"{key}\t{environment[key]}")
    elif args.command == "requires":
        print("\n".join(find_image(manifest, args.alias)["builder"]["requires"]))
    elif args.command == "artifacts":
        artifact = find_image(manifest, args.alias)["artifacts"][args.architecture]
        for key in ("kernel", "initrd", "rootfs"):
            if artifact[key]:
                print(artifact[key])
    elif args.command == "registry-key":
        print(find_image(manifest, args.alias)["artifacts"][args.architecture]["registryKey"])
    elif args.command == "source-registry-key":
        print(source_registry_key(manifest, args.alias, args.architecture))
    elif args.command == "catalog":
        aliases = args.aliases or [image["alias"] for image in manifest["images"]]
        architectures = args.architectures or manifest["architectures"]
        catalog = build_catalog(manifest, args.dist_dir, aliases, architectures)
        rendered = json.dumps(catalog, indent=2, ensure_ascii=False) + "\n"
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            temporary = args.output.with_name(f".{args.output.name}.tmp")
            temporary.write_text(rendered, encoding="utf-8")
            temporary.replace(args.output)
        else:
            print(rendered, end="")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ManifestError as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        raise SystemExit(2)
