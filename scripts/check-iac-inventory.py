#!/usr/bin/env python3
"""Check the frozen v0.4.0 IaC inventory without requiring Git history or a host."""

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = "d426b52ff04223aa0ba9c289fc492448cbb2cb86"
ROUTES_SHA256 = "679dc686c1fb2f517abc97523e83da9d09ea21c62f5aff89552dd8b9d1d10232"


def matches(path: str, namespace: str) -> bool:
    if namespace.startswith("="):
        return path == namespace[1:]
    return path == namespace or path.startswith(namespace + "/")


def check(catalog: dict, root: Path = ROOT) -> list[str]:
    problems = []
    if catalog.get("baseline") != {"tag": "v0.4.0", "commit": BASELINE}:
        problems.append("the service baseline must remain the released v0.4.0 commit")
    resources = catalog.get("resources", [])
    ids = [row["id"] for row in resources]
    if len(ids) != len(set(ids)):
        problems.append("resource IDs must be unique")
    rows = {row["id"]: row for row in resources}
    namespaces = {}
    for row in resources:
        key = row["id"]
        if not re.fullmatch(r"[a-z][a-z0-9_]*", key):
            problems.append(f"{key}: invalid resource ID")
        if row["scope"] not in {"host", "controller", "management"}:
            problems.append(f"{key}: invalid ownership scope")
        if row["stack"] not in {"partial", "missing"} or row["terraform"] not in {"experimental", "missing"}:
            problems.append(f"{key}: baseline support must not be relabeled as target completion")
        for field in ("configuration", "identity", "update", "readiness", "removal"):
            if not row.get(field, "").strip():
                problems.append(f"{key}: missing {field} contract")
        source = (root / row["source"]).resolve()
        if not source.is_relative_to(root.resolve()) or not source.is_file():
            problems.append(f"{key}: missing or out-of-repository source")
        for dependency in row["dependsOn"]:
            if dependency not in rows:
                problems.append(f"{key}: unknown dependency {dependency}")
        for namespace in row["apiNamespaces"]:
            if namespace in namespaces:
                problems.append(f"{key}: duplicate API namespace {namespace}")
            namespaces[namespace] = key

    def visit(key, visiting, visited):
        if key in visiting:
            problems.append(f"{key}: cyclic configuration dependency")
            return
        if key in visited or key not in rows:
            return
        visiting.add(key)
        for dependency in rows[key]["dependsOn"]:
            visit(dependency, visiting, visited)
        visiting.remove(key)
        visited.add(key)

    visited = set()
    for key in rows:
        visit(key, set(), visited)
    paths = catalog.get("baselineRoutes", [])
    if not paths or len(paths) != len(set(paths)):
        problems.append("baseline API paths must be nonempty and unique")
    if hashlib.sha256("\n".join(sorted(paths)).encode()).hexdigest() != ROUTES_SHA256:
        problems.append("the frozen v0.4.0 API paths have changed")
    for source in catalog.get("apiSources", []):
        if not (root / source).is_file():
            problems.append(f"{source}: API source is missing")
    controls = catalog.get("controlRoutes", {})
    for path, reason in controls.items():
        if path not in paths or not reason.strip():
            problems.append(f"{path}: unexplained control route or route absent from baseline")
    for path in paths:
        if path not in controls and not any(matches(path, ns) for ns in namespaces):
            problems.append(f"{path}: no service owns this baseline API family")
    inventory = (root / "public-docs/reference/iac-services.md").read_text()
    documented = re.findall(r"^\| `([a-z][a-z0-9_]*)` \|", inventory, re.MULTILINE)
    if sorted(documented) != sorted(ids):
        problems.append("the public service table must contain every inventory resource exactly once")
    for row in resources:
        lifecycle = "| " + " | ".join(row.get(field, "") for field in
                                     ("id", "identity", "update", "readiness", "removal")) + " |"
        if lifecycle not in inventory:
            problems.append(f"{row['id']}: documented lifecycle differs from the inventory")
    return problems


def main() -> int:
    catalog = json.loads((ROOT / "packaging/iac-v0.4.0.json").read_text())
    problems = check(catalog)
    if problems:
        print("IaC inventory check failed:")
        for problem in problems:
            print(f"  {problem}")
        return 1
    print(f"IaC baseline: {len(catalog['resources'])} resource contracts, "
          f"{len(catalog['baselineRoutes'])} API paths classified; implementation remains tracked in #548.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
