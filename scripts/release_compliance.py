#!/usr/bin/env python3
"""Generate release dependency attribution and enforce a small license policy."""

from __future__ import annotations

import argparse
import json
import re
from collections import deque
from pathlib import Path
from typing import Any

HOST_ROOTS = {"firecrab-api", "firecrab-net-helper", "firecrab-cli"}
NOTICE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "AUTHORS")
BLOCKED_RE = re.compile(r"(?:^|[ (])(?:A?GPL)-", re.IGNORECASE)


def _read_json(path: str | Path) -> dict[str, Any]:
    with open(path, encoding="utf-8") as stream:
        return json.load(stream)


def _packages_by_id(metadata: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {package["id"]: package for package in metadata.get("packages", [])}


def cargo_sets(metadata: dict[str, Any]) -> tuple[set[str], set[str]]:
    """Return external (runtime, build/test-only) Cargo package IDs.

    Runtime traversal follows only normal dependency edges from the three shipped
    workspace binaries. Proc-macro packages are classified as build-only even when
    reached through a normal edge because their code executes during compilation,
    not in the distributed binaries.
    """
    packages = _packages_by_id(metadata)
    workspace = set(metadata.get("workspace_members", []))
    resolve = metadata.get("resolve") or {}
    nodes = {node["id"]: node for node in resolve.get("nodes", [])}

    roots = {
        package_id
        for package_id in workspace
        if packages.get(package_id, {}).get("name") in HOST_ROOTS
    }
    if not roots:
        raise ValueError(
            "Cargo metadata does not contain the shipped FireCrab workspace roots"
        )

    runtime: set[str] = set()
    queue: deque[str] = deque(roots)
    visited: set[str] = set()

    while queue:
        package_id = queue.popleft()
        if package_id in visited:
            continue
        visited.add(package_id)
        node = nodes.get(package_id)
        if not node:
            continue
        for dep in node.get("deps", []):
            kinds = dep.get("dep_kinds") or [{"kind": None}]
            if not any(kind.get("kind") in (None, "normal") for kind in kinds):
                continue
            dep_id = dep["pkg"]
            dep_package = packages.get(dep_id, {})
            target_kinds = {
                kind
                for target in dep_package.get("targets", [])
                for kind in target.get("kind", [])
            }
            if "proc-macro" in target_kinds:
                continue
            if dep_id not in workspace:
                runtime.add(dep_id)
            queue.append(dep_id)

    external = set(packages) - workspace
    return runtime, external - runtime


def npm_sets(
    lock: dict[str, Any],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    packages = lock.get("packages")
    if not isinstance(packages, dict):
        raise ValueError("frontend package-lock must use lockfileVersion 3 packages")

    runtime: list[dict[str, Any]] = []
    build_only: list[dict[str, Any]] = []
    for path, entry in packages.items():
        if not path or not isinstance(entry, dict):
            continue
        record = {
            "path": path,
            "name": _npm_name(path),
            "version": entry.get("version", "unknown"),
            "license": entry.get("license"),
            "source": entry.get("resolved"),
        }
        (build_only if entry.get("dev") else runtime).append(record)
    return runtime, build_only


def _npm_name(path: str) -> str:
    marker = "node_modules/"
    return path.rsplit(marker, 1)[-1] if marker in path else path


def license_allowed(expression: str | None) -> bool:
    """Reject missing/UNLICENSED and GPL/AGPL-only expressions.

    Dual-license expressions pass when at least one OR branch avoids the blocked
    family. LGPL is not treated as GPL here; it carries separate obligations but
    is not an automatic incompatibility for this gate.
    """
    if not expression:
        return False
    expression = expression.strip()
    if expression.upper() in {"UNLICENSED", "SEE LICENSE IN"}:
        return False
    alternatives = re.split(r"\s+OR\s+", expression, flags=re.IGNORECASE)
    return any(
        not BLOCKED_RE.search(alternative.replace("LGPL-", "L-GPL-"))
        for alternative in alternatives
    )


def _license_files(root: Path, explicit: str | None = None) -> list[Path]:
    candidates: list[Path] = []
    if explicit:
        path = root / explicit
        if path.is_file():
            candidates.append(path)
    if root.is_dir():
        for path in sorted(root.iterdir()):
            if path.is_file() and path.name.upper().startswith(NOTICE_PREFIXES):
                candidates.append(path)
    unique: list[Path] = []
    seen: set[Path] = set()
    for path in candidates:
        resolved = path.resolve()
        if resolved not in seen:
            seen.add(resolved)
            unique.append(path)
    return unique


def _read_notice(path: Path) -> str | None:
    try:
        data = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None
    if len(data) > 512_000:
        return None
    return data.rstrip()


def cargo_records(
    metadata: dict[str, Any], runtime_ids: set[str], build_ids: set[str]
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    packages = _packages_by_id(metadata)

    def record(package_id: str) -> dict[str, Any]:
        package = packages[package_id]
        root = Path(package["manifest_path"]).parent
        notices = []
        for path in _license_files(root, package.get("license_file")):
            text = _read_notice(path)
            if text:
                notices.append({"name": path.name, "text": text})
        license_value = package.get("license")
        if not license_value and package.get("license_file"):
            license_value = f"LicenseRef-file:{Path(package['license_file']).name}"
        return {
            "ecosystem": "cargo",
            "name": package["name"],
            "version": package["version"],
            "license": license_value,
            "source": package.get("source") or package.get("repository"),
            "notices": notices,
        }

    return (
        [record(package_id) for package_id in sorted(runtime_ids)],
        [record(package_id) for package_id in sorted(build_ids)],
    )


def npm_records(
    records: list[dict[str, Any]], frontend_root: Path
) -> list[dict[str, Any]]:
    output: list[dict[str, Any]] = []
    for item in records:
        notices = []
        root = frontend_root / item["path"]
        package_json = root / "package.json"
        if not item.get("license") and package_json.is_file():
            try:
                package_data = json.loads(package_json.read_text(encoding="utf-8"))
                declared = package_data.get("license")
                if isinstance(declared, dict):
                    declared = declared.get("type")
                if isinstance(declared, str):
                    item = {**item, "license": declared}
            except (OSError, json.JSONDecodeError):
                pass
        for path in _license_files(root):
            text = _read_notice(path)
            if text:
                notices.append({"name": path.name, "text": text})
        output.append({"ecosystem": "npm", **item, "notices": notices})
    return output


def _sort_key(item: dict[str, Any]) -> tuple[str, str, str]:
    return (item["ecosystem"], item["name"].lower(), str(item["version"]))


def validate_runtime(records: list[dict[str, Any]]) -> list[str]:
    failures = []
    for item in records:
        if not license_allowed(item.get("license")):
            failures.append(
                f"{item['ecosystem']}:{item['name']}@{item['version']}: "
                f"incompatible or missing license {item.get('license')!r}"
            )
    return failures


def render_notices(
    runtime: list[dict[str, Any]], build_only: list[dict[str, Any]]
) -> str:
    lines = [
        "FireCrab third-party notices",
        "============================",
        "",
        "This file is generated from Cargo metadata and the frontend npm lockfile",
        "during the release build. Runtime entries are dependencies reachable by the",
        "distributed host binaries or marked non-dev in the frontend lockfile.",
        "Build/test-only dependencies are inventoried separately at the end.",
        "",
        f"Runtime dependencies: {len(runtime)}",
        f"Build/test-only dependencies: {len(build_only)}",
        "",
    ]
    for item in sorted(runtime, key=_sort_key):
        lines.extend(
            [
                "-" * 78,
                f"{item['ecosystem']} :: {item['name']} {item['version']}",
                f"License: {item.get('license') or 'UNKNOWN'}",
            ]
        )
        if item.get("source"):
            lines.append(f"Source: {item['source']}")
        notices = item.get("notices") or []
        if notices:
            for notice in notices:
                lines.extend(["", f"[{notice['name']}]", notice["text"]])
        else:
            lines.append("Notice text: not present in the installed package payload")
        lines.append("")

    lines.extend(["=" * 78, "Build/test-only inventory", "=" * 78, ""])
    for item in sorted(build_only, key=_sort_key):
        lines.append(
            f"{item['ecosystem']} :: {item['name']} {item['version']} :: "
            f"{item.get('license') or 'UNKNOWN'}"
        )
    return "\n".join(lines).rstrip() + "\n"


def inventory(
    runtime: list[dict[str, Any]], build_only: list[dict[str, Any]]
) -> dict[str, Any]:
    def compact(item: dict[str, Any]) -> dict[str, Any]:
        return {
            key: item.get(key)
            for key in ("ecosystem", "name", "version", "license", "source")
        }

    return {
        "schemaVersion": 1,
        "runtime": [compact(item) for item in sorted(runtime, key=_sort_key)],
        "buildTestOnly": [
            compact(item) for item in sorted(build_only, key=_sort_key)
        ],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cargo-metadata", required=True)
    parser.add_argument("--frontend-lock", required=True)
    parser.add_argument("--frontend-root", required=True)
    parser.add_argument("--notices-out", required=True)
    parser.add_argument("--inventory-out", required=True)
    parser.add_argument("--deny-incompatible", action="store_true")
    args = parser.parse_args(argv)

    cargo = _read_json(args.cargo_metadata)
    lock = _read_json(args.frontend_lock)
    runtime_ids, build_ids = cargo_sets(cargo)
    cargo_runtime, cargo_build = cargo_records(cargo, runtime_ids, build_ids)
    npm_runtime_raw, npm_build_raw = npm_sets(lock)
    npm_runtime = npm_records(npm_runtime_raw, Path(args.frontend_root))
    npm_build = npm_records(npm_build_raw, Path(args.frontend_root))

    runtime = cargo_runtime + npm_runtime
    build_only = cargo_build + npm_build
    failures = validate_runtime(runtime)

    notices_out = Path(args.notices_out)
    inventory_out = Path(args.inventory_out)
    notices_out.parent.mkdir(parents=True, exist_ok=True)
    inventory_out.parent.mkdir(parents=True, exist_ok=True)
    notices_out.write_text(render_notices(runtime, build_only), encoding="utf-8")
    inventory_out.write_text(
        json.dumps(inventory(runtime, build_only), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    print(
        f"release compliance: {len(runtime)} runtime, "
        f"{len(build_only)} build/test-only dependencies"
    )
    if failures:
        for failure in failures:
            print(f"ERROR: {failure}")
        return 1 if args.deny_incompatible else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
