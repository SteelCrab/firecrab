"""Observability acceptance on the disposable Linux Deploy QA host only."""

import http.server
import json
import subprocess
import threading
import time
import uuid


class Collector:
    """Local OTLP/HTTP receiver with a controllable failure response."""

    def __init__(self):
        self.records = []
        self.errors = []
        self.fail = False
        self.lock = threading.Lock()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    assert self.path == "/v1/logs" and self.headers.get_content_type() == "application/json"
                    assert 0 < length <= 1024 * 1024
                    body = json.loads(self.rfile.read(length))
                    with owner.lock:
                        owner.records.append(body)
                        failure = owner.fail
                    response = b"{}"
                    self.send_response(400 if failure else 200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except Exception as error:
                    with owner.lock:
                        owner.errors.append(str(error))
                    self.send_error(400)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def set_failure(self, fail):
        with self.lock:
            self.fail = fail

    def observations(self):
        with self.lock:
            requests = list(self.records)
        output = []
        for request in requests:
            for resource in request["resourceLogs"]:
                attrs = {item["key"]: item["value"]["stringValue"] for item in resource["resource"]["attributes"]}
                for scope in resource["scopeLogs"]:
                    for record in scope["logRecords"]:
                        fields = {item["key"]: item["value"]["stringValue"] for item in record["attributes"]}
                        output.append((attrs, fields, record["body"]["stringValue"]))
        return output

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), "collector did not stop"
        assert not self.errors, self.errors


def run_observation(api, wait_for, traffic, network, spec_a, port, evidence):
    services = []
    collector = Collector()
    errors = []
    metrics = {"stages": [], "cleanupErrors": errors}
    evidence["observability"] = metrics
    started = time.monotonic()

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def snapshot(service):
        return api.call("GET", path(service, "/observability"))

    def log_exists(service, stream, message):
        return any(row["stream"] == stream and message in row["message"] for row in snapshot(service)["logs"])

    def create(label):
        name = "observe-" + uuid.uuid4().hex[:8]
        spec = {**spec_a, "name": name, "domain": name + ".example.test", "replicas": 1,
                "env": {**spec_a.get("env", {}), "OBS_ID": label}}
        service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec}
        services.append(service)
        api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
        service["deployment"] = wait_for(lambda: (record if (record := current(service))["phase"] == "running" else None), timeout=180)
        return service

    def stop_service(service):
        phase = wait_for(lambda: (record if (record := current(service))["phase"] in ["running", "failed", "degraded", "stopped"] else None), timeout=180)["phase"]
        if phase != "stopped":
            api.call("POST", path(service, "/stop"))
            wait_for(lambda: current(service)["phase"] == "stopped", timeout=180)
        api.call("DELETE", path(service))

    def request_log(service):
        assert traffic("127.0.0.1", port, service["spec"]["domain"], "/cgi-bin/observation") == (200, "observed")

    try:
        alpha = create("alpha")
        beta = create("beta")
        for service, label in [(alpha, "alpha"), (beta, "beta")]:
            for stream in ["stdout", "stderr"]:
                wait_for(lambda: log_exists(service, stream, f"fixture-{stream}-{label}"), timeout=60,
                         message=f"{label} {stream} startup output was not collected")
        assert "fixture-stdout-beta" not in json.dumps(snapshot(alpha))
        assert "fixture-stderr-alpha" not in json.dumps(snapshot(beta))
        assert any(event["kind"] == "deployment" for event in snapshot(alpha)["events"])
        assert traffic("127.0.0.1", port, alpha["spec"]["domain"], "/cgi-bin/security-activity") == (200, "security-signal")
        first_release = alpha["deployment"]["id"]
        first_replica = alpha["deployment"]["instances"][0]["vmId"]

        def security(service):
            result = api.call("GET", "/api/service-security")
            return next((item for item in result["services"] if item["id"] == service["id"]), None)

        api.call("POST", "/api/service-security/" + alpha["id"] + "/scan")

        def scan_for_release(service, deployment):
            view = security(service)
            scan = view.get("scan") if view else None
            if scan and scan["status"] == "complete" and scan["deploymentId"] == deployment["id"]:
                assert scan["digest"] == deployment["digest"]
                assert deployment["instances"][0]["vmId"] in scan["replicaIds"]
                assert any(item["cve"] == "CVE-2025-15467" and item["fixedVersion"] == "2.0" for item in scan["findings"])
                return view
            return None

        def first_signal_retained():
            view = security(alpha)
            if not view:
                return None
            assert "qa-secret-log" not in json.dumps(view)
            return view if any(event["deploymentId"] == first_release and first_replica in event["replicaIds"] and event["count"] >= 10 for event in view["activityHistory"]) else None

        wait_for(first_signal_retained, timeout=60, message="security signal was not linked to its service release and replica")
        wait_for(lambda: scan_for_release(alpha, alpha["deployment"]), timeout=120, message="fixture CVE scan did not complete for the running service")
        metrics["stages"].append("fixture CVE scan is tied to the running service release, digest and replica")
        metrics["stages"].append("security signal retains service, release and replica without raw log secrets")
        metrics["stages"].append("isolated stdout, stderr and deployment events")

        result = api.call("PUT", path(alpha, "/observability/otlp"), {"endpoint": collector.endpoint})
        assert result["endpoint"].rstrip("/") == collector.endpoint
        request_log(alpha)
        wait_for(lambda: any(attrs.get("firecrab.service.id") == alpha["id"] and
                             fields.get("log.iostream") == "stderr" and "fixture-cgi-alpha" in body
                             for attrs, fields, body in collector.observations()), timeout=60,
                 message="collector did not receive the service log")
        assert not any("fixture-stdout-alpha" in body for _, _, body in collector.observations()), "pre-opt-in history exported"
        assert not any(attrs.get("firecrab.service.id") == beta["id"] for attrs, _, _ in collector.observations())
        metrics["stages"].append("platform logs exported as OTLP/HTTP JSON after opt-in with service and stream labels")

        old = current(alpha)
        updated_spec = {**alpha["spec"], "env": {**alpha["spec"]["env"], "OBS_REDEPLOY": "1"}}
        update = api.call("POST", path(alpha, "/updates"), {"operationId": str(uuid.uuid4()),
            "expectedReleaseId": old["id"], "spec": updated_spec, "strategy": "rolling"})
        wait_for(lambda: (record if (record := current(alpha))["id"] == update["id"] and record["phase"] == "running" else None), timeout=180,
                 message="observability environment redeploy failed")
        status, app_env = traffic("127.0.0.1", port, alpha["spec"]["domain"], "/cgi-bin/otel-environment")
        assert status == 200
        endpoint, protocol, service_name = app_env.split("|")
        assert endpoint.rstrip("/") == collector.endpoint and protocol == "http/protobuf" and service_name == alpha["spec"]["name"]
        metrics["stages"].append("app OTLP environment applied at new VM boot; app metrics/traces are not generated by this fixture")
        retained = wait_for(first_signal_retained, timeout=60, message="security signal was lost after the release changed")
        assert retained["deploymentId"] == update["id"]
        next_deployment = current(alpha)
        api.call("POST", "/api/service-security/" + alpha["id"] + "/scan")
        scanned = wait_for(lambda: scan_for_release(alpha, next_deployment), timeout=120, message="new release CVE scan did not complete")
        assert any(scan["deploymentId"] == first_release for scan in scanned["previousScans"])
        metrics["stages"].append("fixture CVE scans retain the previous release after redeploy")
        metrics["stages"].append("security signal remains associated with the previous release after redeploy")

        collector.set_failure(True)
        request_log(alpha)
        wait_for(lambda: snapshot(alpha)["otlp"]["failedBatches"] > 0, timeout=60,
                 message="collector failure was not visible")
        collector.set_failure(False)
        metrics["stages"].append("collector failure visible in export status")

        subprocess.run(["systemctl", "stop", "firecrab-api.service"], check=True)
        try:
            address = beta["deployment"]["instances"][0]["ipv4"]
            assert traffic(address, 8080, "fixture", "/cgi-bin/observation") == (200, "observed")
        finally:
            subprocess.run(["systemctl", "start", "firecrab-api.service"], check=True)
        api.ready()
        wait_for(lambda: log_exists(beta, "stderr", "fixture-cgi-beta"), timeout=60,
                 message="output produced while the API was down was not recovered")
        assert "fixture-cgi-beta" not in json.dumps(snapshot(alpha))
        metrics["stages"].append("API restart buffered guest output without cross-service exposure")

        api.call("POST", path(alpha, "/stop"))
        wait_for(lambda: current(alpha)["phase"] == "stopped", timeout=180)
        wait_for(lambda: any(attrs.get("firecrab.service.id") == alpha["id"] and
                             fields.get("firecrab.observation.kind") == "event" and "stopped" in body
                             for attrs, fields, body in collector.observations()), timeout=60,
                 message="collector did not receive the lifecycle event")
        metrics["stages"].append("lifecycle event exported after API restart")
        print("PASS isolated service logs, API restart buffer, events and OTLP export", flush=True)
    except BaseException as error:
        metrics["failure"] = str(error)
        raise
    finally:
        try:
            api.ready()
        except Exception as error:
            errors.append("API readiness: " + str(error))
        for service in reversed(services):
            try:
                stop_service(service)
            except Exception as error:
                errors.append("service cleanup: " + str(error))
        try:
            collector.close()
        except Exception as error:
            errors.append("collector cleanup: " + str(error))
        metrics["elapsedSeconds"] = round(time.monotonic() - started, 2)
        assert not errors, errors
