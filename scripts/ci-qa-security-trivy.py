#!/usr/bin/env python3
"""Deterministic Trivy protocol fixture for disposable real-MicroVM CI only."""
import datetime
import json
import os
import sys

if len(sys.argv) > 1 and sys.argv[1] == "version":
    print(json.dumps({"VulnerabilityDB": {"UpdatedAt": datetime.datetime.now(datetime.timezone.utc).isoformat()}}))
elif len(sys.argv) > 1 and sys.argv[1] == "image":
    if "--download-db-only" in sys.argv:
        sys.exit(0)
    target = sys.argv[-1]
    if not target.startswith("ghcr.io/") or "@sha256:" not in target:
        sys.exit(2)
    with open(os.path.join(os.environ["DOCKER_CONFIG"], "config.json"), encoding="utf-8") as stream:
        config = json.load(stream)
    if list(config.get("auths", {})) != ["ghcr.io"]:
        sys.exit(2)
    report = {"SchemaVersion": 2, "ArtifactName": target, "ArtifactType": "container_image",
              "Metadata": {"RepoDigests": [target], "OS": {"Family": "alpine", "Name": "fixture", "EOSL": False}},
              "Results": [{"Target": "fixture", "Packages": [{"Name": "fixture-package"}],
                           "Vulnerabilities": [{"VulnerabilityID": "CVE-2025-15467", "PkgName": "fixture-package",
                               "InstalledVersion": "1.0", "FixedVersion": "2.0", "Severity": "HIGH",
                               "DataSource": {"Name": "CI fixture", "URL": "https://example.com/advisory"}}]}]}
    print(json.dumps(report))
else:
    sys.exit(2)
