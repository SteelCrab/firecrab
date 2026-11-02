"""Real guest-to-SigNoz acceptance, called only by disposable Deploy QA."""

import contextlib
import http.client
import http.server
import json
import threading
import uuid
from urllib.parse import urlsplit

ROUTE = "/api/observability/signoz"


def local_endpoint(value):
    address = urlsplit(value)
    assert (address.scheme == "http" and address.hostname == "127.0.0.1"
            and address.port and address.port > 1024
            and address.path in ("", "/") and not address.query and not address.fragment
            and not address.username and not address.password), "isolated loopback endpoint required"
    return address


def post(endpoint, path, body):
    address = local_endpoint(endpoint)
    with contextlib.closing(http.client.HTTPConnection(address.hostname, address.port, timeout=15)) as connection:
        connection.request("POST", path, body, {"Content-Type": "application/json"})
        response = connection.getresponse()
        return response.status, response.read()


def indexed_logs(endpoint, service_id):
    identity = str(uuid.UUID(service_id))
    query = ("SELECT concat(body,toJSONString(body_v2)) AS body, toJSONString(tuple(*)) AS record "
             "FROM signoz_logs.logs_v2 "
             f"WHERE position(toJSONString(tuple(*)), '{identity}') > 0 LIMIT 10000 FORMAT JSONEachRow")
    status, data = post(endpoint, "/", query.encode())
    assert status == 200, f"SigNoz indexed-log query failed: HTTP {status} {data[:500]!r}"
    return [json.loads(line) for line in data.splitlines() if line.strip()]


class FaultProxy:
    """Forwards real OTLP batches to SigNoz or returns an injected HTTP 503."""

    def __init__(self, endpoint):
        local_endpoint(endpoint)
        self.fail = False
        self.attempts = 0
        self.lock = threading.Lock()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                if self.path != "/v1/logs" or not 0 < length <= 1024 * 1024:
                    self.send_error(400)
                    return
                body = self.rfile.read(length)
                with owner.lock:
                    owner.attempts += 1
                    failure = owner.fail
                if failure:
                    status, response = 503, b"{}"
                else:
                    status, response = post(endpoint, "/v1/logs", body)
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def set_failure(self, failed):
        with self.lock:
            self.fail = failed
            return self.attempts

    def request_count(self):
        with self.lock:
            return self.attempts

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), "SigNoz fault proxy did not stop"


def run_signoz(api, wait_for, traffic, network, alpha, beta, port, endpoint, query_endpoint, evidence):
    local_endpoint(endpoint)
    local_endpoint(query_endpoint)
    assert api.call("GET", ROUTE)["config"] is None, "refusing an existing SigNoz integration"
    proxy = FaultProxy(endpoint)
    metrics = {"stages": []}
    evidence["signoz"] = metrics
    selected, unselected = alpha["id"], beta["id"]
    owned = False

    def service_path(service, suffix):
        return f"/api/micro-services/{service['id']}{suffix}"

    def source():
        state = api.call("GET", ROUTE)
        return next(row["status"] for row in state["sources"] if row["id"] == selected)

    def configure(paused):
        return api.call("PUT", ROUTE, {"endpoint": proxy.endpoint, "serviceIds": [selected], "paused": paused})

    def rows(service_id=selected):
        return indexed_logs(query_endpoint, service_id)

    def has_body(message):
        return any(message in row["body"] for row in rows())

    def output(service, stage):
        tag = stage + "-" + uuid.uuid4().hex
        assert traffic("127.0.0.1", port, service["spec"]["domain"],
                       "/cgi-bin/observation?" + tag) == (200, "observed")
        return tag

    def collected(service, tag):
        return any(tag in row["message"] for row in
                   api.call("GET", service_path(service, "/observability"))["logs"])

    try:
        # The preceding observation acceptance has stopped alpha and cleared its
        # independent exporter. Its earlier stdout/stderr must not be backfilled.
        probe = api.call("POST", ROUTE + "/test", {"endpoint": proxy.endpoint})
        assert probe["httpStatus"] == 200 and probe["delivered"] == 0
        owned = True
        configure(True)
        api.call("POST", service_path(alpha, "/deployment"), {"microNetworkId": network})
        wait_for(lambda: api.call("GET", service_path(alpha, "/deployment"))["phase"] == "running",
                 timeout=180, message="selected SigNoz source did not start")
        for stream in ["stdout", "stderr"]:
            wait_for(lambda: collected(alpha, f"fixture-{stream}-alpha"), timeout=60)
        wait_for(lambda: source()["queued"] >= 2, timeout=60, message="paused guest logs were not queued")
        assert rows() == [], "paused integration sent records"
        configure(False)
        wait_for(lambda: has_body("fixture-stdout-alpha") and has_body("fixture-stderr-alpha"), timeout=60,
                 message="real guest stdout/stderr were not indexed in SigNoz")
        initial = rows()
        counts = {stream: sum(f"fixture-{stream}-alpha" in row["body"] for row in initial)
                  for stream in ["stdout", "stderr"]}
        assert counts == {"stdout": 1, "stderr": 1}, f"pre-opt-in output was replayed: {counts}"
        wait_for(lambda: any("firecrab.observation.kind" in row["record"] and '"event"' in row["record"]
                            for row in rows()), timeout=60, message="real lifecycle event was not indexed")
        beta_tag = output(beta, "unselected")
        wait_for(lambda: collected(beta, beta_tag), timeout=60)
        assert rows(unselected) == [], "unselected service was exported"
        metrics.update({"selectedService": selected, "unselectedService": unselected,
                        "initialStdout": counts["stdout"], "initialStderr": counts["stderr"],
                        "unselectedRecords": 0, "preOptInRecords": 0})
        metrics["stages"].append("real guest stdout/stderr and lifecycle event indexed; pre-opt-in and unselected records excluded")

        receipt = api.call("POST", ROUTE + "/test-log", {"serviceId": selected})
        assert receipt["httpStatus"] == 200 and receipt["delivered"] == 1
        wait_for(lambda: has_body("Firecrab SigNoz integration test"), timeout=60)
        metrics["stages"].append("explicit test log indexed in SigNoz")

        attempts = proxy.set_failure(True)
        failures = source()["failedBatches"]
        failed_tag = output(alpha, "failed")
        wait_for(lambda: collected(alpha, failed_tag) and source()["failedBatches"] > failures, timeout=60,
                 message="collector failure was not reported")
        assert proxy.request_count() >= attempts + 3, "HTTP 503 retry budget was not exercised"
        assert any(row["httpStatus"] == 503 and row["status"] == "failed" for row in source()["history"])
        proxy.set_failure(False)
        recovery_tag = output(alpha, "recovered")
        wait_for(lambda: has_body(recovery_tag), timeout=60, message="collector recovery was not indexed")
        metrics["stages"].append("HTTP 503 retries and failure history; recovery indexed")

        configure(True)
        wait_for(lambda: source()["inFlight"] == 0, timeout=30)
        paused_tag = output(alpha, "paused")
        wait_for(lambda: collected(alpha, paused_tag) and source()["queued"] > 0, timeout=60)
        assert not has_body(paused_tag), "paused record reached SigNoz"
        configure(False)
        wait_for(lambda: has_body(paused_tag), timeout=60, message="resumed record was not indexed")
        metrics["stages"].append("actual guest output queued while paused and indexed after resume")

        api.call("DELETE", ROUTE)
        assert api.call("GET", ROUTE)["config"] is None
        owned = False
        requests = proxy.request_count()
        disconnected_tag = output(alpha, "disconnected")
        wait_for(lambda: collected(alpha, disconnected_tag), timeout=60)
        assert proxy.request_count() == requests, "new exports after disconnect"
        assert not has_body(disconnected_tag)
        assert has_body(paused_tag), "disconnect deleted records already stored in SigNoz"
        metrics["stages"].append("disconnect stopped exports and preserved indexed data")
        metrics["status"] = "passed"
        print("PASS real MicroService output, events, opt-in isolation, test log, failure/recovery, pause/resume and disconnect in SigNoz", flush=True)
    except BaseException as error:
        metrics["status"] = "failed"
        metrics["failure"] = str(error)
        raise
    finally:
        try:
            if owned:
                api.call("DELETE", ROUTE)
        finally:
            proxy.close()
