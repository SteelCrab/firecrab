#!/usr/bin/env python3
"""Validate real MicroVM domain routing through a temporary public HTTPS tunnel.

Run only against an empty, dedicated Linux/KVM QA API. Creates its own service,
network and Quick Tunnel, and removes them on exit. TLS verification stays enabled.
"""

import argparse
import http.client
import importlib.util
import json
import ssl
import subprocess
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:15335")
    parser.add_argument("--output", type=Path, help="save a JSON report only when explicitly requested")
    parser.add_argument("--restart-unit", default="firecrab-qa335-api.service")
    parser.add_argument("--external-ack", type=Path)
    args = parser.parse_args()
    api = urlsplit(args.api)
    if api.hostname not in {"127.0.0.1", "localhost"} or api.scheme != "http":
        parser.error("use a host-local dedicated QA API")
    if not args.restart_unit.startswith("firecrab-qa335-"):
        parser.error("restart-unit must be a dedicated firecrab-qa335 unit")
    run = uuid.uuid4().hex[:10]
    marker = "FIRECRAB_DOMAIN_" + run.upper()
    report = {"run": run, "marker": marker, "checks": [], "status": "running"}
    services, networks = [], set()
    quick_id = None

    def save():
        if args.output is None:
            return
        args.output.parent.mkdir(parents=True, exist_ok=True)
        temp = args.output.with_suffix(".tmp")
        temp.write_text(json.dumps(report, indent=2))
        temp.replace(args.output)

    def passed(name, **evidence):
        report["checks"].append({"name": name, "passed": True, **evidence})
        save()
        print("PASS: " + name, flush=True)

    def request(method, path, body=None, expected=(200, 201, 202, 204)):
        conn = http.client.HTTPConnection(api.hostname, api.port, timeout=120)
        payload = None if body is None else json.dumps(body)
        try:
            conn.request(method, path, payload, {"Content-Type": "application/json"})
            response = conn.getresponse()
            raw = response.read()
            if response.status not in expected:
                raise RuntimeError(f"{method} {path}: HTTP {response.status}: {raw[:2000]!r}")
            return json.loads(raw) if raw else None
        finally:
            conn.close()

    def public_get(url, path="/health"):
        address = urlsplit(url)
        assert address.scheme == "https" and address.hostname.endswith(".trycloudflare.com")
        conn = http.client.HTTPSConnection(address.hostname, timeout=12, context=ssl.create_default_context())
        try:
            conn.connect()
            tls = conn.sock.version()
            conn.request("GET", path, headers={"Cache-Control": "no-cache"})
            response = conn.getresponse()
            body = response.read().decode()
            if response.status != 200:
                raise RuntimeError(f"public HTTPS: HTTP {response.status}: {body[:200]}")
            return body, tls
        finally:
            conn.close()

    def wait_service(identifier, terminal, timeout=900):
        deadline = time.monotonic() + timeout
        previous = None
        while time.monotonic() < deadline:
            record = request("GET", f"/api/micro-services/{identifier}/deployment")
            if record["phase"] != previous:
                print(f"{identifier}: {record['phase']}", flush=True)
                previous = record["phase"]
            if record["request"].get("microNetworkId"):
                networks.add(record["request"]["microNetworkId"])
            if record["phase"] in terminal:
                return record
            if record["phase"] == "failed":
                raise AssertionError(json.dumps(record, indent=2))
            time.sleep(1)
        raise TimeoutError(f"service did not reach {terminal}")

    def pid(unit):
        return int(subprocess.check_output(["systemctl", "show", "--property=MainPID", "--value", unit], text=True).strip())

    def cleanup():
        nonlocal quick_id
        failures = []
        if quick_id:
            try:
                request("DELETE", f"/api/pipe/quick-tunnels/{quick_id}")
                quick_id = None
            except Exception as error:
                failures.append(str(error))
        for identifier in reversed(services):
            try:
                record = wait_service(identifier, {"running", "failed", "degraded", "stopped"})
                if record["instances"] or record.get("domainRouteOwned"):
                    request("POST", f"/api/micro-services/{identifier}/stop")
                    wait_service(identifier, {"stopped"})
                request("DELETE", f"/api/micro-services/{identifier}")
            except Exception as error:
                failures.append(str(error))
        for identifier in networks:
            try:
                request("DELETE", f"/api/micro-networks/{identifier}")
            except Exception as error:
                failures.append(str(error))
        report["cleanupErrors"] = failures
        if failures:
            raise RuntimeError("QA cleanup incomplete: " + "; ".join(failures))
        pipe = request("GET", "/api/pipe")
        remaining = {"vms": len(request("GET", "/api/vms")), "services": len(request("GET", "/api/micro-services")),
                     "networks": len(request("GET", "/api/micro-networks")), "routes": len((pipe.get("config") or {}).get("routes", [])),
                     "quickTunnels": len(pipe.get("quickTunnels", []))}
        report["remaining"] = remaining
        assert not any(remaining.values()), remaining
        passed("QA services, VMs, network, domain routes and public connector removed", remaining=remaining)

    # Refuse to adopt or clean up somebody else's resources.
    for path in ("/api/vms", "/api/micro-services", "/api/micro-networks"):
        assert request("GET", path) == [], f"QA API must be empty: {path}"
    initial = request("GET", "/api/pipe")
    assert not initial.get("config"), "QA Pipe must not contain existing configuration"
    save()
    try:
        request("PUT", "/api/pipe", {"config": {"listen": "127.0.0.1:18335", "routes": [], "tunnel": None}, "credentials": None})
        snapshot = request("POST", "/api/pipe/quick-tunnels", {"id": None, "name": "qa335-" + run,
                           "target": {"type": "apps", "upstreams": ["127.0.0.1:9"]}})
        entry = next(item for item in snapshot["quickTunnels"] if item["name"] == "qa335-" + run)
        quick_id = entry["id"]
        report["quickTunnelId"] = quick_id
        deadline = time.monotonic() + 240
        while not entry.get("publicUrl"):
            assert time.monotonic() < deadline, entry
            time.sleep(2)
            entry = next(item for item in request("GET", "/api/pipe")["quickTunnels"] if item["id"] == quick_id)
        public_url = entry["publicUrl"]
        domain = urlsplit(public_url).hostname
        report["publicUrl"] = public_url
        save()
        print("PUBLIC_URL: " + public_url, flush=True)

        module_spec = importlib.util.spec_from_file_location("oci_fixture", Path(__file__).with_name("oci-e2e-registry.py"))
        fixture = importlib.util.module_from_spec(module_spec)
        module_spec.loader.exec_module(fixture)
        fixture.READY_COMMAND = """mkdir -p /tmp/www/cgi-bin
printf '%s\\n' "$GREETING" > /tmp/www/health
cat > /tmp/www/cgi-bin/identity <<'CGI'
#!/etc/firecrab/busybox sh
printf 'Content-Type: application/json\\r\\nCache-Control: no-store\\r\\n\\r\\n'
printf '{"marker":"%s","instance":"%s","host":"%s","proto":"%s"}\\n' "$GREETING" "$(/etc/firecrab/busybox cat /sys/class/net/eth0/address)" "$HTTP_HOST" "$HTTP_X_FORWARDED_PROTO"
CGI
/etc/firecrab/busybox chmod 755 /tmp/www/cgi-bin/identity
exec /etc/firecrab/busybox httpd -f -p 8080 -h /tmp/www
"""
        with fixture.LocalOciRegistry() as registry:
            spec = {"name": "qa335-live-" + run, "image": registry.reference, "port": 8080, "ram": 128, "cpu": 1,
                    "replicas": 2, "domain": domain, "healthPath": "/health", "startupTimeout": 15, "drainTimeout": 1,
                    "env": {"GREETING": marker}}
            service = request("POST", "/api/micro-services", spec)
            identifier = service["id"]
            services.append(identifier)
            report["serviceId"] = identifier
            request("POST", f"/api/micro-services/{identifier}/deployment", {})
            deployed = wait_service(identifier, {"running"})
            assert deployed["domainRouteOwned"] and len(deployed["instances"]) == 2, deployed
            assert all(item["healthy"] and item["hostPort"] == 0 for item in deployed["instances"]), deployed
            report["instances"] = deployed["instances"]
            expected = {}
            for instance in deployed["instances"]:
                vm = request("GET", f"/api/vms/{instance['vmId']}")
                expected[vm["mac"].lower()] = vm["id"]
                assert vm["portForwards"] == [], vm
            passed("two real Firecracker VMs deploy without host ports", vmIds=list(expected.values()))
            request("POST", "/api/pipe/quick-tunnels", {"id": quick_id, "name": "qa335-" + run, "target": {"type": "routes", "host": domain}})
            deadline = time.monotonic() + 180
            while True:
                try:
                    body, tls = public_get(public_url)
                    assert body.strip() == marker, body
                    break
                except Exception:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(2)
            passed("public HTTPS verifies its certificate and reaches the service", url=public_url, tls=tls)
            seen = set()
            for _ in range(20):
                body, _ = public_get(public_url, "/cgi-bin/identity")
                identity = json.loads(body)
                assert identity["marker"] == marker and identity["host"] == domain and identity["proto"] == "https", identity
                assert identity["instance"].lower() in expected, identity
                seen.add(identity["instance"].lower())
            assert seen == set(expected), seen
            passed("public requests reach both VMs with the original Host and HTTPS scheme", replicas=sorted(seen))

            units = ["firecrab-ingress.service", entry["unit"]["unit"]] + ["firecrab-vm-" + item["vmId"].replace("-", "") + ".service" for item in deployed["instances"]]
            before = {unit: pid(unit) for unit in units}
            assert all(before.values()), before
            subprocess.run(["systemctl", "stop", args.restart_unit], check=True, timeout=30)
            try:
                for _ in range(5):
                    assert public_get(public_url)[0].strip() == marker
            finally:
                subprocess.run(["systemctl", "start", args.restart_unit], check=True, timeout=30)
            deadline = time.monotonic() + 45
            while True:
                try:
                    recovered = request("GET", f"/api/micro-services/{identifier}/deployment")
                    assert recovered["phase"] == "running", recovered
                    break
                except Exception:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(1)
            assert before == {unit: pid(unit) for unit in units}
            passed("HTTPS and VM processes survive API stop/start and services are re-adopted", processIds=before)

            failed = request("POST", "/api/micro-services", {**spec, "name": "qa335-failed-" + run, "domain": "qa335-" + run + ".example.test", "replicas": 1, "healthPath": "/missing", "startupTimeout": 2})
            services.append(failed["id"])
            request("POST", f"/api/micro-services/{failed['id']}/deployment", {})
            failure = wait_service(failed["id"], {"failed"})
            assert not failure["instances"] and not failure["domainRouteOwned"], failure
            assert public_get(public_url)[0].strip() == marker
            passed("failed app health removes its VM and leaves the public service serving")

            request("POST", f"/api/vms/{deployed['instances'][0]['vmId']}/stop")
            survivor = next(mac for mac, vm_id in expected.items() if vm_id != deployed["instances"][0]["vmId"])
            for _ in range(8):
                body, _ = public_get(public_url, "/cgi-bin/identity")
                assert json.loads(body)["instance"].lower() == survivor, body
            passed("public HTTPS keeps serving after one real VM stops", survivor=survivor)
            report["status"] = "awaiting_external_probe" if args.external_ack else "checks_passed"
            save()
            if args.external_ack:
                deadline = time.monotonic() + 180
                while not args.external_ack.is_file():
                    assert time.monotonic() < deadline, "external HTTPS probe did not acknowledge"
                    time.sleep(1)
                external = json.loads(args.external_ack.read_text())
                assert external["marker"] == marker and external["publicUrl"] == public_url, external
                passed("independent external client verifies public HTTPS", evidence=external)
    except BaseException as error:
        report["status"] = "failed"
        report["error"] = str(error)
        save()
        raise
    finally:
        try:
            cleanup()
        except Exception as error:
            report["status"] = "failed"
            report["cleanupError"] = str(error)
            raise
        finally:
            if report["status"] != "failed":
                report["status"] = "passed"
            save()


if __name__ == "__main__":
    main()
