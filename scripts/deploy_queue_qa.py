"""Queued cancellation and rollback priority on the disposable Deploy QA host."""

import concurrent.futures
import http.client
import subprocess
import threading
import time
import uuid


def run_queue(api, wait_for, traffic, network, spec_a, digest_b, port, evidence):
    services, held_ips, threads = [], [], []
    stop = threading.Event()
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=2)
    started = time.monotonic()
    metrics = {"requests": {}, "httpFailures": [], "monitorErrors": [], "stages": [],
               "maxLiveVMs": 0, "maxLiveVCPUs": 0, "httpTimeoutSeconds": 5}
    evidence["deployQueue"] = metrics

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def operation(service, identifier):
        return api.call("GET", path(service, "/deployments/" + identifier))

    def routes(service):
        return [route for route in api.call("GET", "/api/pipe")["config"]["routes"]
                if (route.get("owner") or {}).get("serviceId") == service["id"]]

    def ready(service):
        record = current(service)
        assert record["phase"] not in ["failed", "stopped", "cancelled"], record
        return record if record["phase"] == "running" else None

    def create(deploy=True):
        name = "queue-" + uuid.uuid4().hex[:8]
        spec = {**spec_a, "name": name, "domain": name + ".example.test", "replicas": 1,
                "drainTimeout": 90}
        service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec}
        services.append(service)
        if deploy:
            api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
            service["initial"] = wait_for(lambda: ready(service), timeout=180)
        return service

    def update(service, previous):
        return api.call("POST", path(service, "/updates"), {
            "operationId": str(uuid.uuid4()), "expectedReleaseId": previous["id"],
            "spec": {**service["spec"], "image": "ghcr.io/team/deploy-qa@" + digest_b},
            "strategy": "rolling"})

    def cancel(service, identifier):
        record = api.call("POST", path(service, "/deployments/" + identifier + "/cancel"))
        assert record["id"] == identifier and record["phase"] == "cancelled", record
        return record

    def hold(service):
        address = current(service)["instances"][0]["ipv4"]
        held_ips.append(address)

        def request():
            # The fixture sends progress bytes. Bound total time rather than
            # relying on the HTTP client's inactivity timeout during cleanup.
            result = subprocess.run(["curl", "--silent", "--show-error", "--max-time", "120",
                                     "--header", "Host: " + service["spec"]["domain"],
                                     "--write-out", "\n%{http_code}",
                                     f"http://127.0.0.1:{port}/cgi-bin/hold"],
                                    capture_output=True, text=True, check=True, timeout=125)
            body, status = result.stdout.rsplit("\n", 1)
            return int(status), body.strip()

        future = executor.submit(request)
        wait_for(lambda: traffic(address, 8080, "fixture", "/cgi-bin/holding") == (200, "holding"), timeout=10)
        return address, future

    def release(address, future, label):
        traffic(address, 8080, "fixture", "/cgi-bin/release")
        assert future.result(timeout=15) == (200, "release-" + label)
        held_ips.remove(address)

    def load(service):
        domain = service["spec"]["domain"]
        metrics["requests"][domain] = 0
        while not stop.is_set():
            try:
                result = traffic("127.0.0.1", port, domain)
                assert result in [(200, "release-A"), (200, "release-B")], result
            except Exception as error:
                if len(metrics["httpFailures"]) < 10:
                    metrics["httpFailures"].append({"domain": domain, "error": str(error)})
            metrics["requests"][domain] += 1
            stop.wait(.03)

    def observe():
        while not stop.is_set():
            try:
                live = [vm for vm in api.call("GET", "/api/vms")
                        if vm["state"] in ["running", "starting", "paused"]]
                cpus = sum(vm["cpu"] for vm in live)
                assert len(live) <= 4 and cpus <= 4, "Deploy queue exceeded the shared surge limit"
                metrics["maxLiveVMs"] = max(metrics["maxLiveVMs"], len(live))
                metrics["maxLiveVCPUs"] = max(metrics["maxLiveVCPUs"], cpus)
            except (OSError, http.client.HTTPException):
                pass  # The API restart does not stop independent ingress traffic.
            except Exception as error:
                if len(metrics["monitorErrors"]) < 10:
                    metrics["monitorErrors"].append(str(error))
            stop.wait(.1)

    def cleanup(service):
        if current(service) is None:
            api.call("DELETE", path(service))
            return
        record = wait_for(lambda: (r if (r := current(service)) and r["phase"] in
                                  ["running", "failed", "degraded", "stopped", "cancelled", "queued"] else None),
                          timeout=180)
        if record["phase"] == "queued":
            cancel(service, record["id"])
            record = current(service)
        if record["phase"] != "cancelled":
            api.call("POST", path(service, "/stop"))
            wait_for(lambda: current(service)["phase"] == "stopped", timeout=180)
        api.call("DELETE", path(service))

    try:
        active, normal, rollback = create(), create(), create()
        previous = rollback["initial"]
        update(rollback, previous)
        rollback_b = wait_for(lambda: ready(rollback), timeout=180)
        for service in [active, normal, rollback]:
            thread = threading.Thread(target=load, args=(service,), daemon=True)
            threads.append(thread)
            thread.start()
        monitor = threading.Thread(target=observe, daemon=True)
        threads.append(monitor)
        monitor.start()
        active_ip, active_hold = hold(active)
        running = update(active, active["initial"])
        wait_for(lambda: current(active)["phase"] == "draining", timeout=180)
        # Nginx drains a whole worker generation. Start the second hold on the
        # new workers so it cannot block completion of the active transition.
        rollback_ip, rollback_hold = hold(rollback)
        assert not active_hold.done() and not rollback_hold.done()

        initial = create(deploy=False)
        queued_initial = api.call("POST", path(initial, "/deployment"), {"microNetworkId": network})
        assert queued_initial["phase"] == "queued"
        cancel(initial, queued_initial["id"])
        cancel(initial, queued_initial["id"])
        assert not current(initial)["instances"] and not routes(initial)
        before = current(normal)
        original_routes = routes(normal)
        queued = update(normal, before)
        assert queued["phase"] == "queued"
        cancel(normal, queued["id"])
        cancel(normal, queued["id"])
        retained = current(normal)
        assert all(retained[key] == before[key] for key in ["id", "spec", "digest", "request"])
        assert {vm["vmId"] for vm in retained["instances"]} == {vm["vmId"] for vm in before["instances"]}
        assert routes(normal) == original_routes
        metrics["cancelledInitial"] = queued_initial["id"]
        metrics["cancelledUpdate"] = queued["id"]
        metrics["stages"].append("queued initial and update cancellation was idempotent; active release and VMs retained")

        next_normal = update(normal, before)
        next_rollback = api.call("POST", path(rollback, "/rollback"), {
            "operationId": str(uuid.uuid4()), "expectedReleaseId": rollback_b["id"],
            "releaseId": previous["id"], "strategy": "rolling"})
        assert next_normal["phase"] == next_rollback["phase"] == "queued"
        metrics["normalOperation"] = next_normal["id"]
        metrics["rollbackOperation"] = next_rollback["id"]
        ingress_pid = api.call("GET", "/api/pipe")["units"][0]["mainPid"]
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
        api.ready()
        assert api.call("GET", "/api/pipe")["units"][0]["mainPid"] == ingress_pid
        assert not active_hold.done() and not rollback_hold.done()
        assert operation(normal, next_normal["id"])["phase"] == "queued"
        assert operation(rollback, next_rollback["id"])["phase"] == "queued"
        assert operation(initial, queued_initial["id"])["phase"] == "cancelled"
        assert operation(normal, queued["id"])["phase"] == "cancelled"
        release(active_ip, active_hold, "A")

        def rollback_draining():
            assert operation(normal, next_normal["id"])["phase"] == "queued", "ordinary Deploy overtook rollback"
            record = operation(rollback, next_rollback["id"])
            assert record["phase"] not in ["failed", "cancelled", "stopped"], record
            return record if record["phase"] == "draining" else None

        wait_for(rollback_draining, timeout=180)
        assert not rollback_hold.done()
        assert current(active)["id"] == running["id"] and current(active)["phase"] == "running"
        metrics["stages"].append("API restart resumed active drain first; rollback overtook the earlier ordinary Deploy")
        release(rollback_ip, rollback_hold, "B")
        wait_for(lambda: ready(rollback), timeout=180)
        wait_for(lambda: ready(normal), timeout=180)
        assert current(rollback)["digest"] == previous["digest"]
        assert current(normal)["digest"] == digest_b
        assert operation(normal, queued["id"])["phase"] == "cancelled"
        assert not metrics["httpFailures"] and not metrics["monitorErrors"], metrics
        assert min(metrics["requests"].values()) > 10, metrics
        metrics["stages"].append("waiting Deploy completed after rollback; cancelled work did not resurrect")
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=10)
        errors = ["traffic monitor did not exit" for thread in threads if thread.is_alive()]
        for address in held_ips:
            try:
                traffic(address, 8080, "fixture", "/cgi-bin/release")
            except Exception as error:
                errors.append("release held request: " + str(error))
        executor.shutdown(wait=True, cancel_futures=True)
        for service in reversed(services):
            try:
                cleanup(service)
            except Exception as error:
                errors.append(str(error))
        metrics["cleanupErrors"] = errors
        metrics["elapsedSeconds"] = round(time.monotonic() - started, 2)
        assert not errors, errors
    assert not metrics["httpFailures"] and not metrics["monitorErrors"], metrics
    print("PASS queued cancellation, rollback priority, API restart and retained HTTP traffic", flush=True)
