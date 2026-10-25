#!/usr/bin/env python3
"""Real MicroVM/Nginx Deploy acceptance on an empty, disposable Linux CI host.

Uses locally built OCI images and a private, fake Auth fixture. It never contacts
GitHub/GHCR and does not establish external-registry or public TLS acceptance.
"""

import argparse
import concurrent.futures
import contextlib
import gzip
import hashlib
import http.client
import http.server
import io
import json
import os
from pathlib import Path
import platform
import socket
import sqlite3
import subprocess
import tarfile
import threading
import time
from urllib.parse import urlsplit
import uuid

from deploy_fleet_qa import host_evidence, run_fleet
from deploy_rotation_qa import run_rotation
from deploy_recovery_qa import run_recovery


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def image_fixture(busybox, label, healthy=True):
    layer = io.BytesIO()
    with tarfile.open(fileobj=layer, mode="w") as archive:
        for directory in ["bin", "www", "www/cgi-bin"]:
            entry = tarfile.TarInfo(directory)
            entry.type = tarfile.DIRTYPE
            entry.mode = 0o755
            archive.addfile(entry)
        files = {"bin/busybox": busybox, "www/index.html": f"release-{label}\n".encode()}
        if healthy:
            files["www/health"] = b"healthy\n"
        for path, data in files.items():
            entry = tarfile.TarInfo(path)
            entry.mode = 0o755 if path == "bin/busybox" else 0o644
            entry.size = len(data)
            archive.addfile(entry, io.BytesIO(data))
        shell = tarfile.TarInfo("bin/sh")
        shell.type = tarfile.SYMTYPE
        shell.linkname = "busybox"
        shell.mode = 0o777
        archive.addfile(shell)
        scripts = {
            "hold": f"""#!/bin/sh
/bin/busybox rm -f /tmp/deploy-release
/bin/busybox touch /tmp/deploy-holding
printf 'Content-Type: text/plain\\r\\n\\r\\n'
while [ ! -f /tmp/deploy-release ]; do printf ' '; /bin/busybox sleep 1; done
printf 'release-{label}\\n'
""",
            "release": "#!/bin/sh\n/bin/busybox touch /tmp/deploy-release\nprintf 'Content-Type: text/plain\\r\\n\\r\\nreleased\\n'\n",
            "holding": "#!/bin/sh\nprintf 'Content-Type: text/plain\\r\\n\\r\\n'\nif [ -f /tmp/deploy-holding ]; then echo holding; else echo idle; fi\n",
            "environment": "#!/bin/sh\nprintf 'Content-Type: text/plain\\r\\n\\r\\n%s:%s\\n' \"$FLEET_SERVICE\" \"$FLEET_RELEASE\"\n",
            "rotation": """#!/bin/sh
printf 'Content-Type: text/plain\\r\\n\\r\\n'
case "$PASSWORD" in
  qa-secret-v0) echo old ;;
  qa-secret-v1) echo next ;;
  qa-secret-v2) echo latest ;;
  *) if [ "$UNRELATED" = 1 ]; then echo unrelated; else echo invalid; fi ;;
esac
""",
            "rotation-health": """#!/bin/sh
case "$PASSWORD:$UNRELATED" in
  qa-secret-v0:*|qa-secret-v1:*|qa-secret-v2:*|:1) printf 'Content-Type: text/plain\\r\\n\\r\\nhealthy\\n' ;;
  *) printf 'Status: 503 Service Unavailable\\r\\nContent-Type: text/plain\\r\\n\\r\\nunhealthy\\n' ;;
esac
""",
            "recovery-health": "#!/bin/sh\nif [ -f /unhealthy ]; then printf 'Status: 503 Service Unavailable\\r\\n'; fi\nprintf 'Content-Type: text/plain\\r\\n\\r\\nhealth\\n'\n",
            "fail-health": "#!/bin/sh\n/bin/busybox touch /unhealthy\nprintf 'Content-Type: text/plain\\r\\n\\r\\ninjected\\n'\n",
            "internal-call": """#!/bin/sh
reply=$(/bin/busybox wget -q -O - "http://$SELF_INTERNAL/cgi-bin/rotation") || {
  printf 'Status: 502 Bad Gateway\\r\\nContent-Type: text/plain\\r\\n\\r\\nunavailable\\n'
  exit 0
}
printf 'Content-Type: text/plain\\r\\n\\r\\n%s\\n' "$reply"
""",
        }
        for name, script in scripts.items():
            data = script.encode()
            entry = tarfile.TarInfo("www/cgi-bin/" + name)
            entry.mode = 0o755
            entry.size = len(data)
            archive.addfile(entry, io.BytesIO(data))
    raw = layer.getvalue()
    compressed = gzip.compress(raw, mtime=0)
    architecture = "arm64" if platform.machine() in ["aarch64", "arm64"] else "amd64"
    config = json.dumps({"architecture": architecture, "os": "linux", "rootfs": {"type": "layers", "diff_ids": [digest(raw)]},
        "config": {"Env": ["PATH=/bin"], "Entrypoint": ["/bin/busybox", "httpd", "-f", "-p", "8080", "-h", "/www"]}}, separators=(",", ":")).encode()
    manifest = json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": digest(config), "size": len(config)},
        "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": digest(compressed), "size": len(compressed)}]}, separators=(",", ":")).encode()
    return digest(manifest), manifest, {digest(config): config, digest(compressed): compressed}


class Registry:
    def __init__(self, fixtures):
        self.fixtures = fixtures
        manifests = {item[0]: item[1] for item in fixtures}
        blobs = {key: value for item in fixtures for key, value in item[2].items()}

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                key = self.path.rsplit("/", 1)[-1]
                body = b"{}" if self.path in ["/v2/", "/v2"] else manifests.get(key, blobs.get(key))
                self.send_response(200 if body is not None else 404)
                body = body if body is not None else b"missing"
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Docker-Content-Digest", digest(body))
                self.send_header("Content-Type", "application/vnd.oci.image.manifest.v1+json" if key in manifests else "application/octet-stream")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def address(self):
        return f"127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def wait_for(check, timeout=180, message="condition timed out"):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError(message)


class Api:
    def __init__(self, base):
        address = urlsplit(base)
        assert address.scheme == "http" and address.hostname == "127.0.0.1", "local disposable API required"
        self.port = address.port or 80

    def call(self, method, path, body=None, allowed=(200, 201, 202, 204)):
        with contextlib.closing(http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)) as connection:
            connection.request(method, path, body=json.dumps(body) if body is not None else None, headers={"Content-Type": "application/json"})
            response = connection.getresponse()
            raw = response.read()
            assert response.status in allowed, f"{method} {path}: {response.status} {raw[:1000]!r}"
            return json.loads(raw) if raw else None

    def ready(self):
        def check():
            try:
                return self.call("GET", "/api/host")
            except (OSError, AssertionError, http.client.HTTPException):
                return None
        return wait_for(check, message="API did not become ready")


def traffic(host, port, domain, path="/"):
    with contextlib.closing(http.client.HTTPConnection(host, port, timeout=120 if path == "/cgi-bin/hold" else 5)) as connection:
        stage = "request"
        try:
            connection.request("GET", path, headers={"Host": domain})
            stage = "response headers"
            response = connection.getresponse()
            stage = "response body"
            return response.status, response.read().decode().strip()
        except TimeoutError as error:
            raise TimeoutError(f"{stage} timed out") from error


def atomic_json(path, value):
    metadata = path.stat()
    temporary = path.with_suffix(".deploy-qa-tmp")
    temporary.write_text(json.dumps(value))
    os.chown(temporary, metadata.st_uid, metadata.st_gid)
    temporary.chmod(metadata.st_mode & 0o777)
    temporary.replace(path)


def require_empty_host(api):
    for resource in ["vms", "micro-services", "pools", "pipe/auth"]:
        assert not api.call("GET", "/api/" + resource), f"refusing existing {resource}"
    unit = "firecrab-ingress.service"
    assert subprocess.check_output(["systemctl", "show", "--property=LoadState", "--value", unit], text=True).strip() == "not-found", "refusing existing ingress"
    assert not api.call("GET", "/api/pipe").get("config"), "refusing existing Pipe configuration"


def run(args, evidence):
    assert args.disposable_host and os.geteuid() == 0 and Path("/dev/kvm").exists()
    required_cpus = 4 if args.secret_rotation or args.replica_recovery else 3
    assert int(subprocess.check_output(["nproc"], text=True)) >= required_cpus, f"{required_cpus} CPUs required for the selected rolling-surge acceptance"
    if args.fifteen_services:
        evidence["host"] = host_evidence(args.images)
    assert "httpd" in subprocess.check_output([args.busybox, "--list"], text=True).splitlines()
    api = Api(args.api)
    api.ready()
    require_empty_host(api)
    if args.secret_rotation or args.replica_recovery:
        assert not api.call("GET", "/api/secrets"), "refusing existing Secrets"
        assert not api.call("GET", "/api/secrets/connections"), "refusing existing Secret connections"
    unit = "firecrab-ingress.service"
    fixtures = [image_fixture(Path(args.busybox).read_bytes(), label, label != "bad") for label in ["A", "B", "bad"]]
    registry = Registry(fixtures)
    images = Path(args.images)
    registrations = images / ".templates.json"
    database = Path(args.database)
    assert database.is_file(), "explicit installed API database required"
    aliases = []
    rootfs_b = None
    saved_rootfs = None
    network = service = None
    ingress_created = auth_seeded = False
    stop = threading.Event()
    pause = threading.Event()
    samples, failures = [], []
    loader = None
    held = []
    held_ips = []
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=12)
    fragment = None
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        ingress_port = reserved.getsockname()[1]
    domain = f"deploy-{uuid.uuid4().hex[:8]}.example.test"

    def service_path(suffix=""):
        return f"/api/micro-services/{service}" + suffix

    def operation(identifier):
        return api.call("GET", service_path(f"/deployments/{identifier}"))

    def terminal(identifier):
        def check():
            current = operation(identifier)
            return current if current["phase"] in ["running", "failed", "stopped"] else None
        return wait_for(check, message=f"Deploy {identifier} did not finish")

    def deploy(spec, previous):
        request = {"operationId": str(uuid.uuid4()), "expectedReleaseId": previous["id"], "spec": spec, "strategy": "rolling"}
        return api.call("POST", service_path("/updates"), request)

    def begin_hold(current):
        nonlocal held_ips
        held_ips = [instance["ipv4"] for instance in current["instances"]]
        pause.set()
        try:
            time.sleep(.1)
            # Pause only fixture setup so round-robin assigns one held request
            # per old replica. Continuous traffic resumes before Deploy starts.
            held.extend(executor.submit(traffic, "127.0.0.1", ingress_port, domain, "/cgi-bin/hold") for _ in held_ips)
            for address in held_ips:
                wait_for(lambda: traffic(address, 8080, domain, "/cgi-bin/holding") == (200, "holding"), timeout=10)
        finally:
            pause.clear()

    def release_holds():
        for address in held_ips:
            traffic(address, 8080, domain, "/cgi-bin/release")
        for future in held:
            assert future.result(timeout=10) == (200, "release-A")
        held.clear()
        held_ips.clear()

    def load():
        while not stop.is_set():
            if pause.is_set():
                stop.wait(.01)
                continue
            try:
                value = traffic("127.0.0.1", ingress_port, domain)
                samples.append(value)
                if value not in [(200, "release-A"), (200, "release-B")]: failures.append(value)
            except Exception as error:
                failures.append(str(error))
            time.sleep(.02)

    def stop_service():
        wait_for(lambda: api.call("GET", service_path("/deployment"))["phase"] in ["running", "failed", "degraded", "stopped"])
        api.call("POST", service_path("/stop"))
        wait_for(lambda: api.call("GET", service_path("/deployment"))["phase"] == "stopped", timeout=180)
        api.call("DELETE", service_path())

    try:
        for digest_value, _, _ in fixtures:
            result = api.call("POST", "/api/oci/import", {"reference": f"{registry.address}/team/deploy-qa@{digest_value}"})
            alias = result["alias"]
            aliases.append(alias)
            result = wait_for(lambda: (value if (value := api.call("GET", f"/api/oci/import/{alias}"))["status"] in ["succeeded", "failed"] else None), timeout=300)
            assert result["status"] == "succeeded", result
        subprocess.run(["systemctl", "stop", "firecrab-api.service"], check=True)
        try:
            entries = json.loads(registrations.read_text())
            for source_alias, (digest_value, _, _) in zip(aliases.copy(), fixtures):
                spec = next(entry.copy() for entry in entries if entry["alias"] == source_alias)
                spec["alias"] = "ghcr.io-team-deploy-qa-sha256-" + digest_value.split(":")[1][:12]
                assert spec["version"] == digest_value
                assert not any(entry["alias"] == spec["alias"] for entry in entries), "refusing an existing fixture alias"
                if digest_value == fixtures[1][0]: rootfs_b = images / spec["rootfs"]
                entries.append(spec)
                aliases.append(spec["alias"])
            atomic_json(registrations, entries)
            with sqlite3.connect(database) as connection:
                assert connection.execute("SELECT count(*) FROM pipe_auth_credentials").fetchone()[0] == 0
                connection.execute("INSERT INTO pipe_auth_credentials(provider, credential) VALUES('github',?)", (json.dumps({"provider": "github", "username": "team", "token": "local-deploy-qa-only"}),))
            auth_seeded = True
        finally:
            subprocess.run(["systemctl", "start", "firecrab-api.service"], check=True)
        api.ready()
        ingress_created = True
        api.call("PUT", "/api/pipe", {"config": {"listen": f"127.0.0.1:{ingress_port}", "routes": [], "tunnel": None}, "credentials": None})
        fragment = subprocess.check_output(["systemctl", "show", "--property=FragmentPath", "--value", unit], text=True).strip()
        network = api.call("POST", "/api/micro-networks", {"name": "deploy-qa-" + uuid.uuid4().hex[:8], "subnetCidr": "172.29.87.0/24", "internetEnabled": True})["id"]
        spec_a = {"name": "deploy-qa-" + uuid.uuid4().hex[:8], "image": "ghcr.io/team/deploy-qa@" + fixtures[0][0], "port": 8080, "healthPath": "/health", "domain": domain, "ram": 128, "cpu": 1, "replicas": 1, "startupTimeout": 10, "drainTimeout": 8, "env": {}, "secrets": {}}
        service = api.call("POST", "/api/micro-services", spec_a)["id"]
        initial = api.call("POST", service_path("/deployment"), {"microNetworkId": network})
        initial = terminal(initial["id"])
        assert initial["phase"] == "running", initial
        assert traffic("127.0.0.1", ingress_port, domain) == (200, "release-A")
        wait_for(lambda: not any(worker["draining"] for worker in api.call("GET", "/api/pipe")["ingressWorkers"]), timeout=10)
        loader = threading.Thread(target=load, daemon=True)
        loader.start()
        begin_hold(initial)
        spec_b = {**spec_a, "image": "ghcr.io/team/deploy-qa@" + fixtures[1][0]}
        updating = deploy(spec_b, initial)
        wait_for(lambda: operation(updating["id"])["phase"] == "draining", timeout=180)
        wait_for(lambda: traffic("127.0.0.1", ingress_port, domain) == (200, "release-B"), timeout=10)
        assert not any(future.done() for future in held)
        ingress_pid = api.call("GET", "/api/pipe")["units"][0]["mainPid"]
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
        api.ready()
        assert api.call("GET", "/api/pipe")["units"][0]["mainPid"] == ingress_pid
        failed_drain = terminal(updating["id"])
        assert failed_drain["phase"] == "failed" and "drainTimeout" in failed_drain["error"], failed_drain
        for instance in initial["instances"]:
            assert api.call("GET", "/api/vms/" + instance["vmId"])["state"] == "running"
        release_holds()
        api.call("POST", service_path("/deployment/retry"), {"operationId": updating["id"]})
        active = terminal(updating["id"])
        assert active["phase"] == "running", active
        print("PASS real single-replica update, retained HTTP request, API restart, drain timeout and retry", flush=True)

        rollback_request = {"operationId": str(uuid.uuid4()), "expectedReleaseId": active["id"], "releaseId": initial["id"]}
        restored = api.call("POST", service_path("/rollback"), rollback_request)
        active = terminal(restored["id"])
        assert active["phase"] == "running" and active["digest"] == fixtures[0][0], active
        assert api.call("POST", service_path("/rollback"), rollback_request)["id"] == active["id"]
        assert traffic("127.0.0.1", ingress_port, domain) == (200, "release-A")
        print("PASS selected digest/configuration rollback and duplicate request identity", flush=True)

        spec_two = {**spec_a, "replicas": 2}
        scaling = deploy(spec_two, active)
        active = terminal(scaling["id"])
        assert active["phase"] == "running" and len(active["instances"]) == 2, active
        begin_hold(active)
        partial = deploy({**spec_b, "replicas": 2, "drainTimeout": 30}, active)
        wait_for(lambda: operation(partial["id"])["phase"] == "draining", timeout=180)
        # The first replacement is admitted. Keep its old workers draining while
        # damaging only this fixture's source cache, then force replacement two to fail.
        saved_rootfs = rootfs_b.with_suffix(".deploy-qa-saved")
        assert not saved_rootfs.exists()
        rootfs_b.rename(saved_rootfs)
        rootfs_b.write_bytes(b"intentional-invalid-qa-rootfs")
        release_holds()
        failed = terminal(partial["id"])
        assert failed["phase"] == "failed" and failed["update"]["completed"], failed
        current = api.call("GET", service_path("/deployment"))
        assert current["id"] == active["id"] and current["phase"] == "running" and len(current["instances"]) == 2, current
        for instance in current["instances"]:
            assert api.call("GET", "/api/vms/" + instance["vmId"])["templateVersion"] == fixtures[0][0]
        saved_rootfs.replace(rootfs_b)
        saved_rootfs = None
        print("PASS partial rolling failure recreates the full previous two-replica release", flush=True)

        bad = deploy({**spec_two, "image": "ghcr.io/team/deploy-qa@" + fixtures[2][0], "startupTimeout": 2}, current)
        failed = terminal(bad["id"])
        assert failed["phase"] == "failed" and failed["update"]["completed"], failed
        current = api.call("GET", service_path("/deployment"))
        assert current["id"] == active["id"] and current["phase"] == "running" and len(current["instances"]) == 2, current
        assert len(samples) > 20 and not failures, {"samples": len(samples), "failures": failures[:10]}
        print(f"PASS unhealthy candidate stays out of traffic; {len(samples)} continuous HTTP requests, zero errors", flush=True)
        evidence["coreRequests"] = len(samples)
        if args.fifteen_services or args.secret_rotation or args.replica_recovery:
            stop.set()
            loader.join(timeout=10)
            stop_service()
            service = None
            if args.secret_rotation:
                run_rotation(api, wait_for, traffic, network, spec_a, ingress_port, evidence)
            elif args.fifteen_services:
                run_fleet(api, wait_for, traffic, network, spec_a, fixtures[1][0], ingress_port, evidence)
            if args.replica_recovery:
                run_recovery(api, wait_for, traffic, network, spec_a, ingress_port, evidence)
    finally:
        stop.set()
        if loader: loader.join(timeout=10)
        for address in held_ips:
            with contextlib.suppress(Exception): traffic(address, 8080, domain, "/cgi-bin/release")
        executor.shutdown(wait=True, cancel_futures=True)
        if saved_rootfs and saved_rootfs.exists(): saved_rootfs.replace(rootfs_b)
        cleanup_errors = []

        def cleanup(label, action):
            try:
                action()
            except Exception as error:
                cleanup_errors.append(f"{label}: {error}")

        if service: cleanup("service VMs and configuration", stop_service)
        if network: cleanup("network", lambda: api.call("DELETE", "/api/micro-networks/" + network))
        for alias in reversed(aliases):
            cleanup("fixture image " + alias, lambda alias=alias: api.call("DELETE", "/api/images/" + alias, allowed=(204, 404)))
        if auth_seeded: cleanup("fixture Auth", lambda: api.call("DELETE", "/api/pipe/auth/github"))
        if ingress_created:
            cleanup("ingress disable", lambda: api.call("POST", "/api/pipe/disable"))
            cleanup("ingress unit", lambda: subprocess.run(["systemctl", "disable", "--now", unit], check=True))
            if fragment: cleanup("ingress fragment", lambda: Path(fragment).unlink(missing_ok=True))
            cleanup("unit reload", lambda: subprocess.run(["systemctl", "daemon-reload"], check=True))
            for name in ["manifest.json", "ingress.conf"]:
                cleanup("Pipe file " + name, lambda name=name: (Path(args.config) / "pipe" / name).unlink(missing_ok=True))
        registry.close()
        assert not cleanup_errors, cleanup_errors
    assert not api.call("GET", "/api/vms") and not api.call("GET", "/api/micro-services")
    evidence["allResourcesRemoved"] = True
    print("PASS all dedicated Deploy QA VMs, services, routes, network, fixture images and credentials removed", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disposable-host", action="store_true", required=True)
    parser.add_argument("--api", default="http://127.0.0.1:5523")
    parser.add_argument("--database", default="/var/lib/firecrab/data/firecrab.db")
    parser.add_argument("--images", default="/var/lib/firecrab/images")
    parser.add_argument("--config", default="/etc/firecrab")
    parser.add_argument("--busybox", default="/usr/bin/busybox")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--fifteen-services", action="store_true", help="require 16 CPUs and run the live 15-service acceptance")
    modes.add_argument("--secret-rotation", action="store_true", help="require four CPUs and run live shared-Secret rotation acceptance")
    parser.add_argument("--replica-recovery", action="store_true", help="run live replica restart/replacement and API recovery acceptance")
    parser.add_argument("--report", type=Path, help="write measured evidence even when acceptance fails")
    args = parser.parse_args()
    if (args.fifteen_services or args.secret_rotation or args.replica_recovery) and not args.report:
        parser.error("extended acceptance requires --report")
    evidence = {"status": "failed", "sourceCommit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()}
    try:
        run(args, evidence)
        evidence["status"] = "passed"
    except BaseException as error:
        evidence["error"] = str(error)
        raise
    finally:
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(evidence, indent=2) + "\n")
