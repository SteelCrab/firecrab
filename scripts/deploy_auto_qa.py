"""Authenticated registry metadata and automatic Deploy on the disposable Linux QA host."""

import base64
import contextlib
import http.server
import http.client
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import tempfile
import threading
import time
import uuid
from urllib.parse import urlsplit


class MetadataRegistry:
    def __init__(self, fixtures, address=("127.0.0.1", 443), tls=None):
        self.tags = {"stable": fixtures[0][0], "v1.0.0": fixtures[0][0], "v1.1.0": fixtures[1][0], "v2.0.0-rc.1": fixtures[3][0]}
        self.manifests = {digest: manifest for digest, manifest, _ in fixtures}
        self.blobs = {key: value for _, _, blobs in fixtures for key, value in blobs.items()}
        self.authenticated = 0
        self.owner = None
        registry = self
        expected = "Basic " + base64.b64encode(b"team:local-deploy-qa-only").decode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.headers.get("Authorization") != expected:
                    self.send_response(401)
                    self.send_header("WWW-Authenticate", 'Basic realm="Deploy QA"')
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                registry.authenticated += 1
                path = urlsplit(self.path).path
                key = path.rsplit("/", 1)[-1]
                manifest = registry.manifests.get(registry.tags.get(key, key))
                if path == "/v2/team/deploy-qa/tags/list":
                    body, mime = json.dumps({"tags": sorted(registry.tags)}).encode(), "application/json"
                elif "/manifests/" in path:
                    body, mime = manifest, "application/vnd.oci.image.manifest.v1+json"
                elif "/blobs/" in path:
                    body, mime = registry.blobs.get(key), "application/octet-stream"
                else:
                    body, mime = b"{}" if path in ["/v2", "/v2/"] else None, "application/json"
                self.send_response(200 if body is not None else 404)
                body = body if body is not None else b"missing"
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Content-Type", mime)
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(address, Handler)
        if tls:
            self.server.socket = tls.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


@contextlib.contextmanager
def trusted_registry(fixtures):
    """Use normal TLS/Auth with a private hosts entry; never contact or publish to GHCR."""
    assert os.geteuid() == 0, "registry trust fixture requires the disposable root QA runner"
    hosts = Path("/etc/hosts")
    assert not any("ghcr.io" in line.split("#", 1)[0].split() for line in hosts.read_text().splitlines()), "refusing an existing GHCR hosts mapping"
    environment = subprocess.check_output(["systemctl", "show", "firecrab-api.service", "--property=Environment", "--value"], text=True)
    assert "SSL_CERT_FILE=" not in environment, "refusing an existing custom registry trust file"
    dropin = Path("/run/systemd/system/firecrab-api.service.d/90-deploy-registry-qa.conf")
    assert not dropin.exists(), "refusing an existing registry QA drop-in"
    directory = Path(tempfile.mkdtemp(prefix="qa-registry-", dir="/var/lib/firecrab"))
    directory.chmod(0o755)
    marker = "127.0.0.1 ghcr.io # firecrab-deploy-qa-" + uuid.uuid4().hex
    registry = None
    appended = installed = False
    try:
        def openssl(*args):
            subprocess.run(["openssl", *args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Deploy QA root", "-keyout", str(directory / "ca.key"), "-out", str(directory / "ca.pem"), "-addext", "basicConstraints=critical,CA:TRUE")
        openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=ghcr.io", "-keyout", str(directory / "leaf.key"), "-out", str(directory / "leaf.csr"))
        (directory / "extensions").write_text("subjectAltName=DNS:ghcr.io\nbasicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
        openssl("x509", "-req", "-days", "1", "-in", str(directory / "leaf.csr"), "-CA", str(directory / "ca.pem"), "-CAkey", str(directory / "ca.key"), "-CAcreateserial", "-extfile", str(directory / "extensions"), "-out", str(directory / "leaf.pem"))
        (directory / "ca.pem").chmod(0o644)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(directory / "leaf.pem", directory / "leaf.key")
        registry = MetadataRegistry(fixtures, tls=tls)  # Binding refuses a pre-existing listener.
        with hosts.open("a") as output:
            output.write("\n" + marker + "\n")
        appended = True
        dropin.parent.mkdir(parents=True, exist_ok=True)
        dropin.write_text(f'[Service]\nEnvironment="SSL_CERT_FILE={directory / "ca.pem"}"\n')
        installed = True
        subprocess.run(["systemctl", "daemon-reload"], check=True)
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
        yield registry
    finally:
        errors = []
        actions = []
        if installed:
            actions.append(dropin.unlink)
        if appended:
            actions.append(lambda: hosts.write_text("\n".join(line for line in hosts.read_text().splitlines() if line != marker) + "\n"))
        if installed:
            actions.extend([lambda: subprocess.run(["systemctl", "daemon-reload"], check=True),
                            lambda: subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)])
        if registry:
            actions.append(registry.close)
        actions.append(lambda: shutil.rmtree(directory))
        for action in actions:
            try:
                action()
            except Exception as error:
                errors.append(error)
        if errors:
            raise ExceptionGroup("registry fixture cleanup failed", errors)


def run_auto(api, wait_for, traffic, network, spec_a, fixtures, port, evidence):
    metrics = {"requests": 0, "httpFailures": [], "monitorErrors": [], "maxLiveVMs": 0, "maxLiveVCPUs": 0,
               "httpTimeoutSeconds": 5, "stages": []}
    evidence["automaticDeploy"] = metrics
    services = []
    stop = threading.Event()
    loader = None
    started = time.monotonic()

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def status(service):
        return api.call("GET", path(service, "/auto-deploy"))

    def ready(service, expected):
        record = current(service)
        assert record["phase"] not in ["failed", "cancelled", "stopped"], record
        return record if record["phase"] == "running" and record["digest"] == expected else None

    def monitor():
        while not stop.is_set():
            for service in services.copy():
                try:
                    value = traffic("127.0.0.1", port, service["spec"]["domain"])
                    metrics["requests"] += 1
                    if value not in [(200, "release-A"), (200, "release-B"), (200, "release-C")]:
                        metrics["httpFailures"].append(value)
                except Exception as error:
                    metrics["httpFailures"].append(str(error))
            try:
                live = [vm for vm in api.call("GET", "/api/vms") if vm["state"] not in ["created", "stopped", "failed"]]
                metrics["maxLiveVMs"] = max(metrics["maxLiveVMs"], len(live))
                metrics["maxLiveVCPUs"] = max(metrics["maxLiveVCPUs"], sum(vm["cpu"] for vm in live))
            except (OSError, http.client.HTTPException):
                pass  # API restart must not interrupt the independently served HTTP traffic.
            except Exception as error:
                metrics["monitorErrors"].append(str(error))
            stop.wait(.05)

    def create():
        name = "auto-" + uuid.uuid4().hex[:8]
        spec = {**spec_a, "name": name, "domain": name + ".example.test", "replicas": 1}
        service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec}
        services.append(service)
        api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
        return service, wait_for(lambda: ready(service, fixtures[0][0]))

    def rollback(service, active, original):
        api.call("POST", path(service, "/rollback"), {"operationId": str(uuid.uuid4()), "expectedReleaseId": active["id"], "releaseId": original["id"]})
        return wait_for(lambda: ready(service, fixtures[0][0]))

    with trusted_registry(fixtures) as registry:
        try:
            api.ready()
            service, original = create()
            unrelated, unrelated_original = create()
            loader = threading.Thread(target=monitor, daemon=True)
            loader.start()
            policy = {"reference": "ghcr.io/team/deploy-qa:stable", "intervalSeconds": 60}
            configured = api.call("PUT", path(service, "/auto-deploy"), policy)
            assert configured["policy"] == policy
            wait_for(lambda: status(service)["observedDigest"] == fixtures[0][0])
            registry.tags["stable"] = fixtures[1][0]
            active = wait_for(lambda: ready(service, fixtures[1][0]), timeout=180)
            assert active["spec"]["image"] == "ghcr.io/team/deploy-qa@" + fixtures[1][0]
            assert status(service)["operationId"] == active["id"]
            metrics["stages"].append("authenticated tag change submitted a pinned automatic Deploy")
            restored = rollback(service, active, original)
            assert status(service)["pause"] == "rollback"
            subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
            api.ready()
            deadline = time.monotonic() + 65
            while time.monotonic() < deadline:
                assert current(service)["id"] == restored["id"]
                assert status(service)["pause"] == "rollback"
                time.sleep(1)
            metrics["stages"].append("rollback suppressed the same digest across API restart and a full poll interval")
            registry.tags["stable"] = fixtures[3][0]
            active = wait_for(lambda: ready(service, fixtures[3][0]), timeout=180)
            assert status(service)["pause"] is None
            metrics["stages"].append("a newly observed digest resumed monitoring")
            restored = rollback(service, active, original)
            api.call("POST", path(service, "/auto-deploy/resume"))
            active = wait_for(lambda: ready(service, fixtures[3][0]), timeout=180)
            assert active["id"] != restored["id"]
            api.call("POST", path(service, "/auto-deploy/pause"))
            assert status(service)["pause"] == "manual"
            api.call("PUT", path(service, "/auto-deploy"), None)
            assert status(service)["policy"] is None
            assert current(unrelated)["id"] == unrelated_original["id"]
            metrics["stages"].append("explicit Resume deployed the same digest; pause/disable left the unrelated service unchanged")
            metrics["authenticatedMetadataRequests"] = registry.authenticated
            assert registry.authenticated > 0
        finally:
            stop.set()
            if loader:
                loader.join(timeout=10)
            for service in services:
                api.ready()
                api.call("POST", path(service, "/stop"))
                wait_for(lambda: current(service)["phase"] == "stopped")
                api.call("DELETE", path(service))
            metrics["durationSeconds"] = round(time.monotonic() - started, 3)
    assert metrics["requests"] > 20 and not metrics["httpFailures"] and not metrics["monitorErrors"], metrics
    assert metrics["maxLiveVMs"] <= 4 and metrics["maxLiveVCPUs"] <= 4, metrics
    api.ready()
    assert not api.call("GET", "/api/vms")
    metrics["cleanup"] = "service VMs, policies, private TLS trust, hosts mapping and registry listener removed"
    print(f"PASS authenticated automatic Deploy, rollback/restart pause and resume; {metrics['requests']} HTTP requests, zero errors", flush=True)
