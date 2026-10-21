#!/usr/bin/env python3
"""Live OCI -> MicroVM -> HTTP service test. Run on the Linux Firecrab host.

The loopback fixture is a real HTTP app served by BusyBox inside Firecracker.
Requires a running API, helper, /dev/kvm, and the normal OCI import tools.
"""

import argparse
import importlib.util
import json
import subprocess
import time
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:5523")
    parser.add_argument("--host-port", type=int, default=30170)
    parser.add_argument("--restart-unit", help="Optional QA API unit to restart after successful deployment")
    parser.add_argument("--keep-running", action="store_true", help="Keep the successful service for manual inspection")
    args = parser.parse_args()
    source = Path(__file__).with_name("oci-e2e-registry.py")
    module_spec = importlib.util.spec_from_file_location("oci_fixture", source)
    fixture = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(fixture)
    fixture.READY_COMMAND = (
        'mkdir -p /tmp/www; printf "%s\\n" "$GREETING" > /tmp/www/health; '
        'cp /tmp/www/health /tmp/www/index.html; '
        'exec /etc/firecrab/busybox httpd -f -p 8080 -h /tmp/www'
    )
    owned = []
    kept = None

    def request(method, path, body=None):
        payload = None if body is None else json.dumps(body).encode()
        req = Request(args.api.rstrip("/") + path, data=payload, method=method)
        if payload is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urlopen(req, timeout=15) as response:
                content = response.read()
                return json.loads(content) if content else None
        except HTTPError as error:
            raise RuntimeError(f"{method} {path}: HTTP {error.code}: {error.read().decode()}") from error

    def wait(service_id, terminal, timeout=600):
        deadline = time.monotonic() + timeout
        previous = None
        while time.monotonic() < deadline:
            status = request("GET", f"/api/micro-services/{service_id}/deployment")
            if status["phase"] != previous:
                print(f"{service_id}: {status['phase']}", flush=True)
                previous = status["phase"]
            if status["phase"] in terminal:
                return status
            if status["phase"] in ("failed", "degraded"):
                raise AssertionError(json.dumps(status, indent=2))
            time.sleep(1)
        raise TimeoutError(f"Service {service_id} did not reach {terminal}")

    def check_http(status):
        for instance in status["instances"]:
            url = f"http://127.0.0.1:{instance['hostPort']}/health"
            with urlopen(url, timeout=5) as response:
                assert response.read().decode().strip() == "FIRECRAB_LIVE_SERVICE_OK"

    def check_removed(service_id):
        prefix = f"ms-{service_id.replace('-', '')}-"
        assert not any(vm["name"].startswith(prefix) for vm in request("GET", "/api/vms"))

    try:
        with fixture.LocalOciRegistry() as registry:
            for suffix, health, replicas in [("healthy", "/health", 2), ("unhealthy", "/missing", 1)]:
                spec = {
                    "name": f"live-{suffix}-{int(time.time())}", "image": registry.reference,
                    "port": 8080, "ram": 128, "cpu": 1, "replicas": replicas,
                    "healthPath": health, "startupTimeout": 3,
                    "drainTimeout": 1,
                    "env": {"GREETING": "FIRECRAB_LIVE_SERVICE_OK"},
                }
                service = request("POST", "/api/micro-services", spec)
                service_id = service["id"]
                owned.append(service_id)
                request("POST", f"/api/micro-services/{service_id}/deployment", {"hostPort": args.host_port + (10 if suffix == "unhealthy" else 0)})
                status = wait(service_id, {"running", "failed"})
                if suffix == "unhealthy":
                    assert status["phase"] == "failed", status
                    assert status["instances"] == [], status
                    assert "health" in status["error"].lower(), status
                    check_removed(service_id)
                    print("PASS: failed app health leaves no service VMs", flush=True)
                    continue
                assert status["phase"] == "running", status
                assert len(status["instances"]) == replicas, status
                assert status["digest"].startswith("sha256:"), status
                for instance in status["instances"]:
                    assert instance["state"] == "running" and instance["healthy"], instance
                check_http(status)
                print("PASS: two real MicroVMs answer HTTP with injected environment", flush=True)
                if args.restart_unit:
                    with subprocess.Popen(["systemctl", "restart", args.restart_unit]) as restart:
                        while True:
                            check_http(status)
                            if restart.poll() is not None:
                                assert restart.returncode == 0
                                break
                            time.sleep(0.05)
                    deadline = time.monotonic() + 45
                    while True:
                        try:
                            status = request("GET", f"/api/micro-services/{service_id}/deployment")
                            assert status["phase"] == "running", status
                            assert all(instance["healthy"] for instance in status["instances"]), status
                            break
                        except (OSError, RuntimeError):
                            if time.monotonic() >= deadline:
                                raise
                            time.sleep(1)
                    print("PASS: API restart re-adopts the service and preserves HTTP health", flush=True)
                kept = service_id
                if not args.keep_running:
                    request("POST", f"/api/micro-services/{service_id}/stop")
                    stopped = wait(service_id, {"stopped"})
                    assert stopped["instances"] == []
                    check_removed(service_id)
                    print("PASS: Stop removes owned VMs and keeps service configuration", flush=True)
                    kept = None
    finally:
        for service_id in owned:
            if args.keep_running and service_id == kept:
                print(f"Kept live service: {service_id}", flush=True)
                continue
            status = request("GET", f"/api/micro-services/{service_id}/deployment")
            if status and status["instances"]:
                if status["phase"] in ("running", "degraded", "failed"):
                    request("POST", f"/api/micro-services/{service_id}/stop")
                    wait(service_id, {"stopped"})
                else:
                    raise RuntimeError(f"Deployment still active; clean up service {service_id} after it finishes")
            request("DELETE", f"/api/micro-services/{service_id}")


if __name__ == "__main__":
    main()
