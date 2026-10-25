"""Real shared-Secret rotation through installed Deploy, KVM and Nginx."""

import concurrent.futures
import contextlib
import http.client
import json
import subprocess
import threading
import time
import uuid


def run_rotation(api, wait_for, traffic, network, spec_a, port, evidence):
    services = []
    secret = None
    held_ip = None
    stop = threading.Event()
    lock = threading.Lock()
    threads = []
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    started = time.monotonic()
    metrics = {"requests": {}, "httpFailures": [], "monitorErrors": [], "maxLiveVMs": 0,
               "maxLiveVCPUs": 0, "stages": []}
    evidence["secretRotation"] = metrics

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def checks():
        with lock:
            assert not metrics["httpFailures"], metrics["httpFailures"]
            assert not metrics["monitorErrors"], metrics["monitorErrors"]

    def ready(service):
        record = current(service)
        assert record["phase"] not in ["failed", "stopped"], record
        return record if record["phase"] == "running" else None

    def edit(value=None):
        summary = next(item for item in api.call("GET", "/api/secrets") if item["id"] == secret)
        body = {"name": summary["name"], "description": "edited by disposable rotation QA", "version": summary["version"]}
        if value is not None:
            body["value"] = value
        return api.call("PUT", "/api/secrets/" + secret, body)

    def load(service):
        domain = service["spec"]["domain"]
        allowed = [(200, "unrelated")] if service["unrelated"] else [(200, label) for label in ["old", "next", "latest"]]
        while not stop.is_set():
            failure = None
            try:
                response = traffic("127.0.0.1", port, domain, "/cgi-bin/rotation")
                if response not in allowed:
                    failure = repr(response)
            except Exception as error:
                failure = str(error)
            with lock:
                metrics["requests"][domain] += 1
                if failure and len(metrics["httpFailures"]) < 10:
                    metrics["httpFailures"].append({"domain": domain, "error": failure})
            stop.wait(.03)

    def observe():
        while not stop.is_set():
            try:
                live = [vm for vm in api.call("GET", "/api/vms") if vm["state"] in ["running", "starting", "paused"]]
                cpus = sum(vm["cpu"] for vm in live)
                assert len(live) <= 4 and cpus <= 4, "Secret rotation exceeded one shared surge replica"
                with lock:
                    metrics["maxLiveVMs"] = max(metrics["maxLiveVMs"], len(live))
                    metrics["maxLiveVCPUs"] = max(metrics["maxLiveVCPUs"], cpus)
            except (OSError, http.client.HTTPException):
                pass  # API restart must not affect the separate HTTP measurements.
            except Exception as error:
                with lock:
                    if len(metrics["monitorErrors"]) < 10:
                        metrics["monitorErrors"].append(str(error))
            stop.wait(.1)

    def latest():
        checks()
        records = [current(service) for service in services]
        if any(record["phase"] != "running" for record in records):
            return None
        for service, record in zip(services, records):
            expected = "unrelated" if service["unrelated"] else "latest"
            if traffic("127.0.0.1", port, service["spec"]["domain"], "/cgi-bin/rotation") != (200, expected):
                return None
            assert record["spec"] == service["spec"] and record["digest"] == spec_a["image"].split("@", 1)[1], record
            public = json.dumps(record) + json.dumps(api.call("GET", path(service, "/releases")))
            assert not any(value in public for value in ["qa-secret-v0", "qa-secret-v1", "qa-secret-v2", "qa-secret-bad"])
        return records

    try:
        secret = api.call("POST", "/api/secrets", {"name": "rotation-qa-" + uuid.uuid4().hex[:8], "value": "qa-secret-v0"})["id"]
        for index in range(3):
            name = f"rotate-{index}-" + uuid.uuid4().hex[:8]
            unrelated = index == 2
            spec = {**spec_a, "name": name, "domain": name + ".example.test", "healthPath": "/cgi-bin/rotation-health",
                    "startupTimeout": 4, "drainTimeout": 0 if unrelated else 45,
                    "env": {"UNRELATED": "1"} if unrelated else {},
                    "secrets": {} if unrelated else {"PASSWORD": secret}}
            service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec, "unrelated": unrelated}
            services.append(service)
            metrics["requests"][spec["domain"]] = 0
            api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
            service["initial"] = wait_for(lambda: ready(service), timeout=180)
            service["spec"] = service["initial"]["spec"].copy()
        for target, arguments in [(observe, ())] + [(load, (service,)) for service in services]:
            thread = threading.Thread(target=target, args=arguments, daemon=True)
            threads.append(thread)
            thread.start()
        initial_ids = [service["initial"]["id"] for service in services]
        initial_vms = {vm["id"] for vm in api.call("GET", "/api/vms")}
        edit()  # A metadata edit increments Secret version but must not roll any VM.
        time.sleep(12)
        checks()
        assert [current(service)["id"] for service in services] == initial_ids
        assert {vm["id"] for vm in api.call("GET", "/api/vms")} == initial_vms
        metrics["stages"].append("metadata edit retained all replicas")

        first = services[0]
        held_ip = first["initial"]["instances"][0]["ipv4"]
        held = executor.submit(traffic, "127.0.0.1", port, first["spec"]["domain"], "/cgi-bin/hold")
        wait_for(lambda: traffic(held_ip, 8080, "fixture", "/cgi-bin/holding") == (200, "holding"), timeout=10)
        edit("qa-secret-v1")
        wait_for(lambda: current(first)["phase"] == "draining", timeout=180)
        assert not held.done()
        assert traffic("127.0.0.1", port, first["spec"]["domain"], "/cgi-bin/rotation") == (200, "next")
        edit("qa-secret-v2")
        edit("qa-secret-v2")
        pid = api.call("GET", "/api/pipe")["units"][0]["mainPid"]
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
        api.ready()
        assert api.call("GET", "/api/pipe")["units"][0]["mainPid"] == pid
        traffic(held_ip, 8080, "fixture", "/cgi-bin/release")
        assert held.result(timeout=10) == (200, "release-A")
        held_ip = None
        rotated = wait_for(latest, timeout=300, message="shared Secret values did not converge after restart")
        assert all(rotated[i]["id"] != initial_ids[i] for i in [0, 1])
        assert rotated[2]["id"] == initial_ids[2]
        assert rotated[2]["instances"] == services[2]["initial"]["instances"]
        metrics["stages"].append("shared rotation, rapid edits, pinned image, restart, retained HTTP, unrelated VM preservation")
        print("PASS shared Secret rotation and rapid edits recover after API restart; image/config and unrelated VM retained", flush=True)

        stable_vms = {vm["id"] for vm in api.call("GET", "/api/vms")}
        edit("qa-secret-bad")
        def bad_attempt():
            checks()
            record = current(first)
            return record if record["id"] != rotated[0]["id"] and record["phase"] == "checking_health" else None
        attempted = wait_for(bad_attempt, timeout=180)
        # Restore desired values while the captured bad candidate is still probing.
        # That attempt must keep its private snapshot and fail without another retry race.
        edit("qa-secret-v2")
        def failed():
            record = api.call("GET", path(first, "/deployments/" + attempted["id"]))
            return record if record["phase"] == "failed" else None
        rejected = wait_for(failed, timeout=180)
        assert rejected["update"]["completed"], rejected
        wait_for(latest, timeout=180)
        time.sleep(12)
        records = latest()
        assert records and [record["id"] for record in records] == [record["id"] for record in rotated]
        assert {vm["id"] for vm in api.call("GET", "/api/vms")} == stable_vms
        api.call("DELETE", "/api/secrets/" + secret, allowed=(409,))
        metrics["stages"].append("unhealthy Secret candidate rejected, previous values/VMs retained, in-use deletion rejected")
        checks()
        assert metrics["maxLiveVMs"] == 4 and metrics["maxLiveVCPUs"] == 4, metrics
        assert min(metrics["requests"].values()) > 20, metrics
    except BaseException as error:
        metrics["scenarioError"] = str(error)
        raise
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=35)
        if held_ip:
            with contextlib.suppress(Exception):
                traffic(held_ip, 8080, "fixture", "/cgi-bin/release")
        executor.shutdown(wait=True, cancel_futures=True)
        errors = []
        for service in services:
            try:
                for _ in range(3):
                    api.call("POST", path(service, "/stop"))
                    result = wait_for(lambda: (record if (record := current(service))["phase"] in ["stopped", "failed"] else None), timeout=180)
                    if result["phase"] == "stopped":
                        break
                    time.sleep(1)
                assert result["phase"] == "stopped", result
                api.call("DELETE", path(service))
            except Exception as error:
                errors.append(str(error))
        if secret:
            try:
                api.call("DELETE", "/api/secrets/" + secret)
            except Exception as error:
                errors.append(str(error))
        metrics["elapsedSeconds"] = round(time.monotonic() - started, 2)
        metrics["cleanupErrors"] = errors
        assert not errors, errors
    checks()
    print(f"PASS Secret rotation: {sum(metrics['requests'].values())} continuous HTTP requests, zero errors; dedicated services/Secret removed", flush=True)
