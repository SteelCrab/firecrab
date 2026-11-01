"""Real PostgreSQL service-security acceptance on the disposable MicroVM host."""

import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import uuid


PASSWORD = "firecrab-db-qa-only"
BAD_PASSWORD = "incorrect-qa-password"
SOURCE = "PostgreSQL JSON authentication logs"


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def image_fixture(archive_path):
    """Convert a locally built Docker save archive to verified OCI registry blobs."""
    max_blob = 512 * 1024 * 1024
    total = 0
    with tarfile.open(archive_path, "r") as archive:
        def read(path):
            nonlocal total
            entry = archive.getmember(path)
            if not entry.isfile() or entry.size > max_blob:
                raise ValueError("invalid PostgreSQL fixture archive member")
            total += entry.size
            if total > 512 * 1024 * 1024:
                raise ValueError("PostgreSQL fixture archive is too large")
            return archive.extractfile(entry).read()

        saved = json.loads(read("manifest.json"))
        if len(saved) != 1:
            raise ValueError("expected one PostgreSQL fixture image")
        image = saved[0]
        config = read(image["Config"])
        config_digest = hashlib.sha256(config).hexdigest()
        if Path(image["Config"]).name not in (config_digest, config_digest + ".json"):
            raise ValueError("PostgreSQL image config digest mismatch")
        parsed = json.loads(config)
        arch = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(platform.machine())
        if parsed.get("os") != "linux" or parsed.get("architecture") != arch or arch is None:
            raise ValueError("PostgreSQL fixture architecture mismatch")
        if parsed.get("config", {}).get("Entrypoint") != ["/usr/local/bin/firecrab-postgres-qa"]:
            raise ValueError("unexpected PostgreSQL fixture entrypoint")
        paths = image["Layers"]
        expected = parsed.get("rootfs", {}).get("diff_ids", [])
        if not paths or len(paths) != len(expected) or len(paths) > 64:
            raise ValueError("invalid PostgreSQL fixture layer count")
        blobs = {digest(config): config}
        layers = []
        for path, expected_diff in zip(paths, expected):
            blob = read(path)
            if blob.startswith(b"\x1f\x8b"):
                raw = gzip.decompress(blob)
            else:
                raw = blob
                blob = gzip.compress(raw, mtime=0)
            if len(raw) > max_blob or digest(raw) != expected_diff:
                raise ValueError("PostgreSQL fixture layer digest mismatch")
            key = digest(blob)
            blobs[key] = blob
            layers.append({"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": key, "size": len(blob)})
        manifest = json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                               "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": digest(config), "size": len(config)},
                               "layers": layers}, separators=(",", ":")).encode()
        return digest(manifest), manifest, blobs


def run_database_security(api, wait_for, network, spec_a, fixture, evidence):
    """Link and scan a PostgreSQL MicroVM, then collect actual failed logins."""
    metrics = {"stages": [], "cleanupErrors": []}
    evidence["databaseSecurity"] = metrics
    service = connection = None
    original_error = False
    try:
        assert not api.call("GET", "/api/pipe/databases"), "refusing existing database connections"
        name = "security-postgres-" + uuid.uuid4().hex[:8]
        spec = {**spec_a, "name": name, "image": "ghcr.io/team/deploy-qa@" + fixture[0],
                "domain": name + ".example.test", "ram": 1024, "replicas": 1,
                "healthPath": "/cgi-bin/health", "startupTimeout": 90,
                "env": {"POSTGRES_PASSWORD": PASSWORD}, "secrets": {}}
        service = api.call("POST", "/api/micro-services", spec)["id"]
        path = "/api/micro-services/" + service
        api.call("POST", path + "/deployment", {"microNetworkId": network})

        def running():
            record = api.call("GET", path + "/deployment")
            if record["phase"] == "failed":
                raise AssertionError("PostgreSQL MicroVM deployment failed")
            return record if record["phase"] == "running" else None

        deployment = wait_for(running, timeout=300, message="PostgreSQL MicroVM did not start")
        assert len(deployment["instances"]) == 1
        replica = deployment["instances"][0]["vmId"]
        address = deployment["instances"][0]["ipv4"]
        assert deployment["digest"] == fixture[0]
        connection = api.call("POST", "/api/pipe/databases", {"name": name, "provider": "local",
            "uri": f"postgres://postgres:{PASSWORD}@{address}:5432/postgres", "tlsMode": "disable"})["id"]
        before = api.call("GET", "/api/service-security")
        assert any(item["id"] == connection and item["reason"] == "database_connection_unlinked" for item in before["unsupportedWorkloads"])
        api.call("PUT", f"/api/service-security/databases/{connection}/link", {"serviceId": service})
        linked = api.call("GET", "/api/service-security")
        assert any(item["id"] == connection and item["serviceId"] == service for item in linked["databases"])
        assert not any(item["id"] == connection for item in linked["unsupportedWorkloads"])
        metrics["stages"].append("live PostgreSQL connection linked to its running MicroVM service")

        api.call("POST", f"/api/service-security/{service}/scan")
        def scanned():
            coverage = api.call("GET", "/api/service-security")
            row = next((item for item in coverage["services"] if item["id"] == service), None)
            scan = row.get("scan") if row else None
            if scan and scan["status"] == "complete":
                assert scan["deploymentId"] == deployment["id"] and scan["digest"] == fixture[0]
                assert replica in scan["replicaIds"]
                return row
            return None
        wait_for(scanned, timeout=180, message="real PostgreSQL image CVE scan did not finish")
        metrics["stages"].append("real PostgreSQL image CVE scan retains service, release, digest and replica")

        env = {**os.environ, "PGPASSWORD": BAD_PASSWORD, "PGCONNECT_TIMEOUT": "5", "PGSSLMODE": "disable"}
        for _ in range(10):
            result = subprocess.run(["psql", "-w", "-h", address, "-p", "5432", "-U", "postgres",
                                     "-d", "postgres", "-c", "SELECT 1"], env=env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=10)
            assert result.returncode != 0 and "password authentication failed" in result.stderr, "database did not reject invalid credentials"

        def detected():
            coverage = api.call("GET", "/api/service-security")
            text = json.dumps(coverage)
            assert PASSWORD not in text and BAD_PASSWORD not in text and address not in text, "security API leaked database connection details"
            row = next((item for item in coverage["services"] if item["id"] == service), None)
            if row and any(event["source"] == SOURCE and event["deploymentId"] == deployment["id"]
                           and replica in event["replicaIds"] and event["count"] >= 10
                           for event in row["activityHistory"]):
                return row
            return None
        wait_for(detected, timeout=120, message="real PostgreSQL authentication failures were not detected")
        metrics["stages"].append("real PostgreSQL authentication failures retain service, release and replica without connection details")
        api.call("DELETE", f"/api/service-security/databases/{connection}/link")
        unlinked = api.call("GET", "/api/service-security")
        assert any(item["id"] == connection and item["reason"] == "database_connection_unlinked" for item in unlinked["unsupportedWorkloads"])
        metrics["stages"].append("unlink restores explicit unsupported database coverage")
        print("PASS real PostgreSQL MicroVM connection, image CVE scan and authentication detection", flush=True)
    except BaseException as error:
        original_error = True
        metrics["failure"] = str(error)
        raise
    finally:
        if connection:
            try:
                api.call("DELETE", "/api/pipe/databases/" + connection, allowed=(200, 204, 404))
                assert not any(item["id"] == connection for item in api.call("GET", "/api/pipe/databases")), "database connection was not removed"
            except Exception as error:
                metrics["cleanupErrors"].append("database connection: " + str(error))
        if service:
            try:
                path = "/api/micro-services/" + service
                record = api.call("GET", path + "/deployment", allowed=(200, 404))
                if isinstance(record, dict) and record.get("phase") not in (None, "stopped"):
                    wait_for(lambda: (current if (current := api.call("GET", path + "/deployment"))["phase"] in ("running", "failed", "degraded", "stopped") else None), timeout=180)
                    api.call("POST", path + "/stop")
                    wait_for(lambda: api.call("GET", path + "/deployment")["phase"] == "stopped", timeout=180)
                api.call("DELETE", path, allowed=(204, 404))
            except Exception as error:
                metrics["cleanupErrors"].append("database service: " + str(error))
        if metrics["cleanupErrors"] and not original_error:
            raise RuntimeError("database security QA cleanup failed; see report")
