#!/usr/bin/env python3
"""Deploy/stop real MicroServices and check the first post-Stop DNS response.

Use an empty, dedicated Linux/KVM QA API with Pipe Auth already connected.
The image must serve HTTP on the supplied port and health path. The caller
template must provide a serial root shell and /etc/firecrab/busybox.
"""

import argparse
import importlib.util
import ipaddress
import json
from pathlib import Path
import re
import subprocess
import time
from urllib.parse import urlsplit
from urllib.request import ProxyHandler, Request, build_opener
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:15480")
    parser.add_argument("--image", required=True)
    parser.add_argument("--caller-template", required=True)
    parser.add_argument("--port", type=int, default=80)
    parser.add_argument("--health-path", default="/")
    parser.add_argument("--subnet", default="172.29.48.0/24")
    parser.add_argument("--uplink", default="qa-uplink")
    parser.add_argument("--cycles", type=int, default=2)
    parser.add_argument("--dns-deadline", type=float, default=1.0)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.cycles < 1 or args.dns_deadline <= 0:
        parser.error("cycles and DNS deadline must be positive")
    address = urlsplit(args.api)
    if address.scheme != "http" or address.hostname != "127.0.0.1":
        parser.error("use a dedicated loopback QA API")
    opener = build_opener(ProxyHandler({}))
    report = {"status": "running", "checks": [], "dns": [], "cycles": args.cycles}
    services, networks, callers = [], [], []
    tag = "qa480-" + uuid.uuid4().hex[:8]

    def save():
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2))

    def request(path, method="GET", body=None):
        data = json.dumps(body).encode() if body is not None else (b"" if method == "POST" else None)
        req = Request(args.api.rstrip("/") + path, data=data, method=method,
                      headers={"Content-Type": "application/json"})
        with opener.open(req, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def until(check, seconds=180):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = check()
            if result:
                return result
            time.sleep(0.05)
        raise AssertionError("QA condition timed out")

    def passed(name, **evidence):
        report["checks"].append({"name": name, **evidence})
        save()
        print("PASS " + name, flush=True)

    def deployment(service, phase):
        def ready():
            state = request(f"/api/micro-services/{service['id']}/deployment")
            assert state["phase"] != "failed", state.get("error")
            return state if state["phase"] == phase else None
        return until(ready)

    module = importlib.util.spec_from_file_location("qa_console", Path(__file__).with_name("ci-qa-vm-pause.py"))
    console_module = importlib.util.module_from_spec(module)
    module.loader.exec_module(console_module)

    class Console(console_module.Console):
        def command(self, text):
            token = uuid.uuid4().hex[:12]
            self.text = ""
            self.send("echo BEGIN" + token + "; " + text + "; echo END" + token + "$?\n")
            until(lambda: (self.read(0.01), re.search(r"\nEND" + token + r"\d+\r?\n", self.text))[1], 30)
            output = self.text.split("\r\nBEGIN" + token + "\r\n")[-1]
            with args.output.with_suffix(".console.txt").open("a") as file:
                file.write(output)
            return output

    for path in ["/api/vms", "/api/micro-services", "/api/micro-networks"]:
        assert request(path) == [], "refusing a non-empty QA API: " + path
    save()
    console = None
    try:
        network = request("/api/micro-networks", "POST", {"name": tag, "subnetCidr": args.subnet,
                          "internetEnabled": False, "uplink": args.uplink})
        networks.append(network["id"])
        gateway = str(next(ipaddress.ip_network(args.subnet).hosts()))

        def gateway_mac():
            links = json.loads(subprocess.check_output(["ip", "-j", "address", "show"], text=True))
            return next(link["address"] for link in links
                        if any(item.get("local") == gateway for item in link.get("addr_info", [])))

        initial_mac = gateway_mac()
        report["gatewayMac"] = initial_mac
        caller = request("/api/vms", "POST", {"name": tag + "-caller", "template": args.caller_template,
                         "cpu": 1, "ram": 128, "diskGb": 1, "microNetworkId": network["id"]})
        callers.append(caller["id"])
        request(f"/api/vms/{caller['id']}/start", "POST")
        until(lambda: request(f"/api/vms/{caller['id']}")["state"] == "running")
        assert gateway_mac() == initial_mac, "caller TAP changed gateway MAC"
        console = Console(address.port, caller["id"])
        console.send("\n")

        def prompt():
            console.read(0.05)
            if "login:" in console.text and "# " not in console.text:
                console.send("root\n")
            return "# " in console.text
        until(prompt, 30)

        def create_service(suffix, replicas):
            service = request("/api/micro-services", "POST", {"name": tag + suffix, "image": args.image,
                              "port": args.port, "healthPath": args.health_path, "ram": 128, "cpu": 1,
                              "replicas": replicas, "startupTimeout": 90, "drainTimeout": 1})
            services.append(service)
            return service

        def deploy(service):
            request(f"/api/micro-services/{service['id']}/deployment", "POST", {"microNetworkId": network["id"]})
            result = deployment(service, "running")
            assert result["internalRouteOwned"] and all(i["healthy"] for i in result["instances"])
            assert all(i["hostPort"] == 0 for i in result["instances"])
            assert gateway_mac() == initial_mac, "deployment changed gateway MAC"
            return result

        keep = create_service("-keep", 1)
        deploy(keep)
        target = create_service("-target", 2)
        for cycle in range(args.cycles):
            current = deploy(target)
            host = target["name"] + ".internal"
            dns = console.command("/etc/firecrab/busybox nslookup " + host)
            assert gateway in dns and "NXDOMAIN" not in dns
            http = console.command("/etc/firecrab/busybox wget -T 2 -S -O /tmp/qa480-http http://" + host + args.health_path)
            assert "200 OK" in http, http
            neighbor = console.command("/etc/firecrab/busybox ip neigh show " + gateway)
            assert initial_mac in neighbor.lower(), neighbor
            passed(f"cycle {cycle + 1}: real deployment registers DNS and HTTP route",
                   replicas=len(current["instances"]), digest=current["digest"], gatewayMac=gateway_mac())

            request(f"/api/micro-services/{target['id']}/stop", "POST")
            stopped = deployment(target, "stopped")
            assert not stopped["internalRouteOwned"] and stopped["instances"] == []
            assert gateway_mac() == initial_mac, "Stop changed gateway MAC"
            for query in ["", "-type=A ", "-type=AAAA "]:
                start = time.monotonic()
                output = console.command("/etc/firecrab/busybox nslookup " + query + host)
                elapsed = time.monotonic() - start
                report["dns"].append({"cycle": cycle + 1, "query": query.strip() or "A+AAAA",
                                      "seconds": elapsed, "nxdomain": "NXDOMAIN" in output})
                save()
                assert "NXDOMAIN" in output and elapsed < args.dns_deadline, (elapsed, output)
            survivor = console.command("/etc/firecrab/busybox wget -T 2 -S -O /tmp/qa480-keep http://"
                                       + keep["name"] + ".internal" + args.health_path)
            assert "200 OK" in survivor, survivor
            passed(f"cycle {cycle + 1}: first DNS queries meet deadline and other service stays reachable",
                   gatewayMac=gateway_mac())
        report["status"] = "passed"
    finally:
        failures = []
        for service in reversed(services):
            try:
                state = request(f"/api/micro-services/{service['id']}/deployment")
                if state and state["phase"] != "stopped":
                    request(f"/api/micro-services/{service['id']}/stop", "POST")
                    deployment(service, "stopped")
                request(f"/api/micro-services/{service['id']}", "DELETE")
            except Exception as error:
                failures.append(str(error))
        if console is not None:
            console.socket.close()
        for identifier in callers:
            try:
                request(f"/api/vms/{identifier}/stop", "POST")
                request(f"/api/vms/{identifier}", "DELETE")
            except Exception as error:
                failures.append(str(error))
        for identifier in networks:
            try:
                request(f"/api/micro-networks/{identifier}", "DELETE")
            except Exception as error:
                failures.append(str(error))
        report["cleanupErrors"] = failures
        if report["status"] != "passed" or failures:
            report["status"] = "failed"
        save()
        assert not failures, failures


if __name__ == "__main__":
    main()
