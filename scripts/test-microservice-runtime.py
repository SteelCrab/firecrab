#!/usr/bin/env python3
"""Live OCI -> MicroVM -> HTTP service test. Run on the Linux Firecrab host.

Requires a running API, helper, /dev/kvm and a fixture image from an account
connected in Pipe Auth. The image must serve HTTP on port 8080, return the
GREETING environment value at /health and a non-2xx response at /missing.
"""

import argparse
import json
import subprocess
import time
import uuid
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:5523")
    parser.add_argument("--image", required=True, help="Fixture image in a Pipe Auth-connected GHCR namespace or ECR registry")
    parser.add_argument("--host-port", type=int, default=30170)
    parser.add_argument("--restart-unit", help="Optional QA API unit to restart after successful deployment")
    parser.add_argument("--keep-running", action="store_true", help="Keep the successful service for manual inspection")
    parser.add_argument("--with-secrets", action="store_true", help="Verify encrypted Secret injection, redaction, rotation isolation and VM restart")
    args = parser.parse_args()
    owned = []
    kept = None
    secret = None
    greeting = "FIRECRAB_LIVE_SERVICE_OK"
    if args.with_secrets:
        greeting += "_" + uuid.uuid4().hex

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
                assert response.read().decode().strip() == greeting

    def check_redaction(service_id, status):
        for payload in [status, request("GET", f"/api/micro-services/{service_id}"), request("GET", "/api/vms")]:
            assert greeting not in json.dumps(payload), "Secret leaked through a public response"

    def wait_vm(vm_id, expected):
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            vm = request("GET", f"/api/vms/{vm_id}")
            if vm["state"] == expected:
                return
            assert vm["state"] not in ("failed", "crashed"), "replica lifecycle failed"
            time.sleep(1)
        raise TimeoutError("replica did not reach " + expected)

    def check_removed(service_id):
        prefix = f"ms-{service_id.replace('-', '')}-"
        assert not any(vm["name"].startswith(prefix) for vm in request("GET", "/api/vms"))

    try:
        if args.with_secrets:
            secret = request("POST", "/api/secrets", {"name": "qa-service-" + uuid.uuid4().hex, "value": greeting})
        for suffix, health, replicas in [("healthy", "/health", 2), ("unhealthy", "/missing", 1)]:
            spec = {
                "name": f"live-{suffix}-{int(time.time())}", "image": args.image,
                "port": 8080, "ram": 128, "cpu": 1, "replicas": replicas,
                "healthPath": health, "startupTimeout": 30 if suffix == "healthy" else 3,
                "drainTimeout": 1,
                "env": {} if secret else {"GREETING": greeting},
                "secrets": {"GREETING": secret["id"]} if secret else {},
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
            if secret:
                check_redaction(service_id, status)
                request("PUT", "/api/secrets/" + secret["id"], {
                    "name": secret["name"], "value": "rotated-" + uuid.uuid4().hex, "version": secret["version"],
                })
                vm_id = status["instances"][0]["vmId"]
                request("POST", f"/api/vms/{vm_id}/stop")
                wait_vm(vm_id, "stopped")
                request("POST", f"/api/vms/{vm_id}/start")
                wait_vm(vm_id, "running")
                deadline = time.monotonic() + 60
                while True:
                    try:
                        check_http(status)
                        break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(1)
                check_redaction(service_id, request("GET", f"/api/micro-services/{service_id}/deployment"))
                print("PASS: Secret values stay private and VM restart retains the deployment snapshot after rotation", flush=True)
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
        if secret and not (args.keep_running and kept):
            request("DELETE", "/api/secrets/" + secret["id"])


if __name__ == "__main__":
    main()
