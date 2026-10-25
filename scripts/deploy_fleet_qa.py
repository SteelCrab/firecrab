"""Fifteen simultaneously running services on one real, disposable KVM host."""

import concurrent.futures
import contextlib
import hashlib
import http.client
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time
import uuid


def host_evidence(images):
    cpus = int(subprocess.check_output(["nproc"], text=True))
    assert cpus >= 16, "15 services plus one rolling surge require at least 16 CPUs"
    memory = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
    available = int(memory["MemAvailable"].split()[0]) // 1024
    free_disk = shutil.disk_usage(images).free
    assert available >= 6400, "16 replicas require 6400 MiB available including VMM and host reserves"
    assert free_disk >= 16 * 1024**3, "16 GiB of free replica disk headroom required"
    binaries = {}
    for name in ["firecrab-api", "firecrab-helper", "firecrab-ingress", "firecracker"]:
        path = Path(shutil.which(name) or Path("/usr/local/lib/firecrab") / name)
        binaries[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"cpus": cpus, "availableMemoryMiB": available, "freeDiskBytes": free_disk,
            "kernel": os.uname().release, "binarySha256": binaries}


def run_fleet(api, wait_for, traffic, network, spec_a, digest_b, port, evidence):
    services = []
    stop = threading.Event()
    lock = threading.Lock()
    errors = []
    held = None
    hold_ip = None
    monitor = None
    loaders = []
    metrics = {"serviceCount": 15, "maxLiveVMs": 0, "maxLiveVCPUs": 0,
               "maxActiveDeploys": 0, "maxQueuedDeploys": 0, "observations": 0,
               "requests": {}, "httpFailures": [], "stages": []}
    evidence["fleet"] = metrics
    metrics["microNetworkId"] = network
    terminal_phases = {"running", "failed", "stopped"}
    started = time.monotonic()
    hold_executor = concurrent.futures.ThreadPoolExecutor(max_workers=1)

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def attempt(service):
        return api.call("GET", path(service, "/deployments/" + service["operation"]))

    def observe():
        while not stop.is_set():
            try:
                vms = api.call("GET", "/api/vms")
                live = [vm for vm in vms if vm["state"] in ["running", "starting", "paused"]]
                assert len(live) <= 16, f"rolling surge exceeded 16 live VMs: {len(live)}"
                cpus = sum(vm["cpu"] for vm in live)
                assert cpus <= 16, f"rolling surge exceeded 16 vCPUs: {cpus}"
                records = [(service, attempt(service)["phase"]) for service in services if service.get("operation")]
                phases = [phase for _, phase in records]
                active_services = [service for service, phase in records if phase not in terminal_phases | {"queued"}]
                # Separate GETs can straddle a permit handoff. Recheck overlap.
                active = sum(attempt(service)["phase"] not in terminal_phases | {"queued"} for service in active_services)
                queued = phases.count("queued")
                assert active <= 1, f"shared deployment permit exceeded: {phases}"
                with lock:
                    for key, value in [("maxLiveVMs", len(live)), ("maxLiveVCPUs", cpus),
                                       ("maxActiveDeploys", active), ("maxQueuedDeploys", queued)]:
                        metrics[key] = max(metrics[key], value)
                    metrics["observations"] += 1
            except (OSError, http.client.HTTPException):
                # The API is deliberately restarted. HTTP traffic is measured independently.
                pass
            except Exception as error:
                with lock:
                    errors.append(str(error))
                stop.set()
            stop.wait(.1)

    def load(service):
        domain = service["spec"]["domain"]
        while not stop.is_set():
            try:
                response = traffic("127.0.0.1", port, domain)
                failure = None if response in [(200, "release-A"), (200, "release-B")] else repr(response)
            except Exception as error:
                failure = str(error)
            with lock:
                metrics["requests"][domain] += 1
                if failure and len(metrics["httpFailures"]) < 10:
                    metrics["httpFailures"].append({"domain": domain, "error": failure})
            stop.wait(.05)

    def check_monitor():
        with lock:
            assert not errors, errors
            assert not metrics["httpFailures"], metrics["httpFailures"]

    def finished():
        check_monitor()
        records = [attempt(service) for service in services]
        failed = [record for record in records if record["phase"] in ["failed", "stopped"]]
        assert not failed, failed
        return records if all(record["phase"] == "running" for record in records) else None

    def verify(records, digest_value, label, stage):
        vms = api.call("GET", "/api/vms")
        assert len(vms) == 15 and all(vm["state"] == "running" for vm in vms), vms
        assert len({vm["id"] for vm in vms}) == 15
        for service, record in zip(services, records):
            assert record["digest"] == digest_value and len(record["instances"]) == 1, record
            assert record["request"]["microNetworkId"] == network, record
            assert record["spec"]["name"] == service["spec"]["name"], record
            assert record["spec"]["domain"] == service["spec"]["domain"], record
            assert record["spec"]["env"] == {**service["spec"]["env"], "FLEET_RELEASE": label}, record
            assert traffic("127.0.0.1", port, service["spec"]["domain"]) == (200, "release-" + label)
            assert traffic("127.0.0.1", port, service["spec"]["domain"], "/cgi-bin/environment") == (
                200, service["spec"]["env"]["FLEET_SERVICE"] + ":" + label)
            service["active"] = record
        metrics["stages"].append({"name": stage, "runningServices": 15, "digest": digest_value})
        print(f"PASS fleet {stage}: 15 simultaneously running KVM services and owned Nginx domains", flush=True)

    def submit(service, action):
        previous = service["active"]
        body = {"operationId": str(uuid.uuid4()), "expectedReleaseId": previous["id"], "strategy": "rolling"}
        if action == "updates":
            body["spec"] = {**service["spec"], "image": "ghcr.io/team/deploy-qa@" + digest_b,
                            "env": {**service["spec"]["env"], "FLEET_RELEASE": "B"}}
        else:
            body["releaseId"] = service["initial"]["id"]
        record = api.call("POST", path(service, "/" + action), body)
        service["operation"] = record["id"]

    try:
        for index in range(15):
            name = "fleet-" + uuid.uuid4().hex[:8]
            spec = {**spec_a, "name": name, "domain": name + ".example.test",
                    "startupTimeout": 30, "drainTimeout": 90,
                    "env": {"FLEET_SERVICE": str(index), "FLEET_RELEASE": "A"}}
            service = api.call("POST", "/api/micro-services", spec)
            services.append({"id": service["id"], "spec": spec})
            metrics["requests"][spec["domain"]] = 0
        monitor = threading.Thread(target=observe, daemon=True)
        monitor.start()
        with concurrent.futures.ThreadPoolExecutor(max_workers=15) as executor:
            def initial(service):
                record = api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
                service["operation"] = record["id"]
            list(executor.map(initial, services))
            records = wait_for(finished, timeout=900, message="15 initial Deploys did not finish")
            verify(records, spec_a["image"].split("@", 1)[1], "A", "initial")
            for service in services:
                service["initial"] = service["active"]
                loader = threading.Thread(target=load, args=(service,), daemon=True)
                loaders.append(loader)
                loader.start()
            wait_for(lambda: not any(worker["draining"] for worker in api.call("GET", "/api/pipe")["ingressWorkers"]))
            hold_ip = services[0]["active"]["instances"][0]["ipv4"]
            held = hold_executor.submit(traffic, "127.0.0.1", port, services[0]["spec"]["domain"], "/cgi-bin/hold")
            wait_for(lambda: traffic(hold_ip, 8080, "fixture", "/cgi-bin/holding") == (200, "holding"), timeout=10)
            list(executor.map(lambda service: submit(service, "updates"), services))
            wait_for(lambda: any(attempt(service)["phase"] == "draining" for service in services), timeout=180)
            queued = sum(attempt(service)["phase"] == "queued" for service in services)
            assert queued > 0 and not held.done(), "restart must interrupt active drain with queued Deploys"
            pid = api.call("GET", "/api/pipe")["units"][0]["mainPid"]
            subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
            api.ready()
            assert api.call("GET", "/api/pipe")["units"][0]["mainPid"] == pid, "Nginx was replaced by API restart"
            traffic(hold_ip, 8080, "fixture", "/cgi-bin/release")
            assert held.result(timeout=10) == (200, "release-A")
            hold_ip = None
            metrics["restart"] = {"queuedDeploys": queued, "nginxPidPreserved": True, "oldRequestCompleted": True}
            records = wait_for(finished, timeout=900, message="15 queued updates did not recover after API restart")
            verify(records, digest_b, "B", "update-after-restart")

            service = services[0]
            old_ids = {vm["id"] for vm in api.call("GET", "/api/vms")}
            rejected = api.call("POST", path(service, "/updates"), {
                "operationId": str(uuid.uuid4()), "expectedReleaseId": service["active"]["id"],
                "strategy": "rolling", "spec": {**service["active"]["spec"], "replicas": 16, "cpu": 2}})
            def capacity_rejected():
                record = api.call("GET", path(service, "/deployments/" + rejected["id"]))
                return record if record["phase"] in terminal_phases else None
            rejected = wait_for(capacity_rejected)
            assert rejected["phase"] == "failed" and "CPU" in rejected["error"] and rejected["update"]["completed"], rejected
            assert api.call("GET", path(service, "/deployment"))["id"] == service["active"]["id"]
            assert {vm["id"] for vm in api.call("GET", "/api/vms")} == old_ids
            metrics["capacityRejection"] = {"error": rejected["error"], "all15VMIdentitiesPreserved": True}
            print("PASS excessive CPU/surge request rejected while all 15 active services remain", flush=True)

            list(executor.map(lambda service: submit(service, "rollback"), services))
            records = wait_for(finished, timeout=900, message="15 selected rollbacks did not finish")
            verify(records, spec_a["image"].split("@", 1)[1], "A", "selected-rollback")
            for service in services:
                history = api.call("GET", path(service, "/releases"))
                assert service["initial"]["id"] in {release["id"] for release in history}, history
                assert any(release["digest"] == digest_b for release in history), history
            metrics["services"] = [{"id": service["id"], "domain": service["spec"]["domain"],
                                    "initialReleaseId": service["initial"]["id"],
                                    "rollbackReleaseId": service["active"]["id"]} for service in services]
        check_monitor()
        assert metrics["maxQueuedDeploys"] > 0 and metrics["maxActiveDeploys"] == 1, metrics
        assert metrics["maxLiveVMs"] == 16 and metrics["maxLiveVCPUs"] == 16, metrics
        assert min(metrics["requests"].values()) >= 20, metrics
    except BaseException as error:
        metrics["scenarioError"] = str(error)
        raise
    finally:
        stop.set()
        for thread in loaders + ([monitor] if monitor else []):
            thread.join(timeout=35)
        if hold_ip:
            with contextlib.suppress(Exception):
                traffic(hold_ip, 8080, "fixture", "/cgi-bin/release")
        hold_executor.shutdown(wait=True, cancel_futures=True)
        cleanup_errors = []
        for service in services:
            try:
                wait_for(lambda: api.call("GET", path(service, "/deployment"))["phase"] in terminal_phases | {"degraded"}, timeout=900)
                api.call("POST", path(service, "/stop"))
                wait_for(lambda: api.call("GET", path(service, "/deployment"))["phase"] == "stopped", timeout=180)
                api.call("DELETE", path(service))
            except Exception as error:
                cleanup_errors.append({"service": service["id"], "error": str(error)})
        metrics["elapsedSeconds"] = round(time.monotonic() - started, 2)
        metrics["cleanupErrors"] = cleanup_errors
        assert not cleanup_errors, cleanup_errors
    check_monitor()
    print(f"PASS fleet: {sum(metrics['requests'].values())} continuous HTTP requests, zero errors; all 15 services removed", flush=True)
