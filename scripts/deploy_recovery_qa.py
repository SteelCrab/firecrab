"""Real replica exit, unhealthy application, replacement and restart acceptance."""

import concurrent.futures
import contextlib
import json
import ipaddress
import subprocess
import threading
import time
import uuid


def run_recovery(api, wait_for, traffic, network, spec_a, port, evidence):
    services = []
    secret = None
    held_ip = None
    stop = threading.Event()
    errors = []
    metrics = {"requests": {}, "httpFailures": [], "stages": []}
    evidence["replicaRecovery"] = metrics
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    loaders = []
    started = time.monotonic()

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def ready(service):
        record = current(service)
        return record if record["phase"] == "running" and not (record.get("recovery") or {}).get("repair") else None

    gateway = str(ipaddress.ip_network(api.call("GET", "/api/micro-networks/" + network)["subnetCidr"]).network_address + 1)

    def create(replicas, private=False):
        name = "repair-" + uuid.uuid4().hex[:8]
        spec = {**spec_a, "name": name, "domain": None if private else name + ".example.test", "replicas": replicas,
                "healthPath": "/cgi-bin/recovery-health", "startupTimeout": 3, "drainTimeout": 30,
                "env": {**spec_a.get("env", {}), "SELF_INTERNAL": name + ".internal"},
                "secrets": {"PASSWORD": secret}}
        service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec}
        services.append(service)
        api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
        service["initial"] = wait_for(lambda: ready(service), timeout=180)
        return service

    def load(service, internal=False):
        domain = service["spec"]["domain"]
        key = domain + ("/guest-internal" if internal else "")
        metrics["requests"][key] = 0
        while not stop.is_set():
            try:
                response = traffic("127.0.0.1", port, domain, "/cgi-bin/internal-call" if internal else "/cgi-bin/rotation")
                if response != (200, "old"):
                    raise AssertionError(repr(response))
            except Exception as error:
                if len(metrics["httpFailures"]) < 10:
                    metrics["httpFailures"].append({"domain": domain, "error": str(error)})
            metrics["requests"][key] += 1
            stop.wait(.03)

    def ids(record):
        return {instance["vmId"] for instance in record["instances"]}

    def stop_service(service):
        wait_for(lambda: current(service)["phase"] in ["running", "degraded", "failed", "stopped"], timeout=180)
        api.call("POST", path(service, "/stop"))
        wait_for(lambda: current(service)["phase"] == "stopped", timeout=180)

    try:
        secret = api.call("POST", "/api/secrets", {"name": "recovery-" + uuid.uuid4().hex[:8], "value": "qa-secret-v0"})["id"]
        main, unrelated = create(2), create(1)
        for service in [main, unrelated]:
            for internal in [False, True]:
                thread = threading.Thread(target=load, args=(service, internal), daemon=True)
                loaders.append(thread)
                thread.start()
        original = main["initial"]
        unrelated_ids = ids(unrelated["initial"])
        victim = original["instances"][0]["vmId"]
        api.call("POST", "/api/vms/" + victim + "/stop")
        wait_for(lambda: (record if (record := ready(main)) and ids(record) == ids(original)
                         and api.call("GET", "/api/vms/" + victim)["state"] == "running" else None), timeout=180)
        metrics["stages"].append("exited VM restarted; healthy replicas, release, image and captured Secret retained")

        held = executor.submit(traffic, "127.0.0.1", port, main["spec"]["domain"], "/cgi-bin/hold")

        def holding():
            for instance in current(main)["instances"]:
                if traffic(instance["ipv4"], 8080, "fixture", "/cgi-bin/holding") == (200, "holding"):
                    return instance
            return None

        target = wait_for(holding, timeout=15)
        held_ip = target["ipv4"]
        traffic(held_ip, 8080, "fixture", "/cgi-bin/fail-health")

        def draining():
            record = current(main)
            repair = (record.get("recovery") or {}).get("repair")
            return record if repair and repair.get("transition") and repair["transition"]["confirmed"] else None

        wait_for(draining, timeout=60)
        assert not held.done(), "failed replica was stopped before its held request drained"
        ingress_pid = api.call("GET", "/api/pipe")["units"][0]["mainPid"]
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
        api.ready()
        assert api.call("GET", "/api/pipe")["units"][0]["mainPid"] == ingress_pid
        assert not held.done()
        traffic(held_ip, 8080, "fixture", "/cgi-bin/release")
        assert held.result(timeout=15) == (200, "release-A")
        held_ip = None

        def replaced():
            record = ready(main)
            return record if record and target["vmId"] not in ids(record) else None

        repaired = wait_for(replaced, timeout=240)
        assert len(ids(repaired)) == 2 and len(ids(repaired) & ids(original)) == 1
        assert repaired["id"] == original["id"] and repaired["digest"] == original["digest"]
        assert repaired["spec"] == original["spec"] and repaired["request"] == original["request"]
        assert ids(current(unrelated)) == unrelated_ids
        routes = [route for route in api.call("GET", "/api/pipe")["config"]["routes"]
                  if (route.get("owner") or {}).get("serviceId") == main["id"]]
        expected = sorted(instance["ipv4"] + ":8080" for instance in repaired["instances"])
        assert len(routes) == 2 and all(sorted(route["upstreams"]) == expected for route in routes), routes
        assert target["vmId"] not in {vm["id"] for vm in api.call("GET", "/api/vms")}
        assert "qa-secret-v0" not in json.dumps(repaired)
        metrics["stages"].append("unhealthy VM replaced after restart failed; held request and repair journal survived API restart")

        single = create(1, private=True)
        only = single["initial"]["instances"][0]
        traffic(only["ipv4"], 8080, "fixture", "/cgi-bin/fail-health")
        wait_for(lambda: traffic(gateway, 80, single["spec"]["name"] + ".internal")[0] == 503, timeout=60)
        wait_for(lambda: (record if (record := ready(single)) and ids(record) != ids(single["initial"]) else None), timeout=180)
        metrics["stages"].append("all-unready service kept its hostname with HTTP 503 and recovered to a ready replacement")
        stop_service(single)
        time.sleep(16)
        assert current(single)["phase"] == "stopped" and not current(single)["instances"]
        assert ids(current(unrelated)) == unrelated_ids
        assert not metrics["httpFailures"], metrics["httpFailures"]
        assert min(metrics["requests"].values()) > 50
        metrics["stages"].append("Stop was retained; unrelated service and healthy replica IDs were preserved")
        print("PASS replica restart, replacement, API recovery, bounded ownership and Nginx availability", flush=True)
    finally:
        stop.set()
        for thread in loaders:
            thread.join(timeout=10)
        if held_ip:
            with contextlib.suppress(Exception):
                traffic(held_ip, 8080, "fixture", "/cgi-bin/release")
        executor.shutdown(wait=True, cancel_futures=True)
        for service in reversed(services):
            try:
                stop_service(service)
                api.call("DELETE", path(service))
            except Exception as error:
                errors.append(str(error))
        if secret:
            try:
                api.call("DELETE", "/api/secrets/" + secret)
            except Exception as error:
                errors.append(str(error))
        metrics["cleanupErrors"] = errors
        metrics["elapsedSeconds"] = round(time.monotonic() - started, 2)
        assert not errors, errors
