#!/usr/bin/env python3
"""Exercise generated PipeIngress config against real Nginx, without root."""

import argparse
import concurrent.futures
import contextlib
import http.client
import http.server
import json
import os
import re
import shutil
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit


QUICK_HOST = "quick.firecrab.internal"
APP_QUICK_ID = "11111111-1111-4111-8111-111111111111"
ROUTES_QUICK_ID = "22222222-2222-4222-8222-222222222222"
SECOND_APP_QUICK_ID = "33333333-3333-4333-8333-333333333333"
API_ONLY_QUICK_ID = "44444444-4444-4444-8444-444444444444"


def quick_host(identifier):
    return "quick-" + identifier.replace("-", "") + ".firecrab.internal"

QUICK_URL = re.compile(r"\|\s*(https://[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.trycloudflare\.com)\s*\|", re.IGNORECASE)


class Backend:
    def __init__(self, name):
        self.name = name
        self.healthy = True
        self.slow_started = threading.Event()
        self.failures = 0
        backend = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/slow":
                    backend.slow_started.set()
                    time.sleep(0.5)
                status = 200 if backend.healthy else 503
                if status == 503:
                    backend.failures += 1
                origin = {"host": self.headers.get("Host"), "forwardedHost": self.headers.get("X-Forwarded-Host"), "forwardedProto": self.headers.get("X-Forwarded-Proto")}
                body = json.dumps(origin).encode() if self.path in {"/headers", "/api/headers"} else f"{backend.name}:{self.path}".encode()
                if self.path == "/redirect" and status == 200:
                    status = 302
                self.send_response(status)
                if self.path in {"/headers", "/api/headers"}:
                    self.send_header("Content-Type", "application/json")
                if status == 302:
                    self.send_header("Location", f"{origin['forwardedProto']}://{origin['host']}/landing")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def address(self):
        return f"127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def request(port, path="/", host="app.example.com", headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
    try:
        connection.request("GET", path, headers={"Host": host, **(headers or {})})
        response = connection.getresponse()
        return response.status, response.read().decode()
    finally:
        connection.close()


def redirect_location(port, host, headers):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
    try:
        connection.request("GET", "/redirect", headers={"Host": host, **headers})
        response = connection.getresponse()
        response.read()
        assert response.status == 302, response.status
        return response.getheader("Location")
    finally:
        connection.close()


def live_quick_tunnel(cloudflared, port, directory, timeout, doh_url=None, internal_host=QUICK_HOST, expected_paths=None):
    """Expose one disposable Nginx target, verify its paths, then stop the connector."""
    curl = shutil.which("curl")
    assert curl, "optional public HTTPS QA requires curl"
    expected_paths = expected_paths or {"/": {"one", "two"}}
    connector_config = directory / "quick-cloudflared.json"
    connector_config.write_text("{}\n")
    log_path = directory / (internal_host + ".cloudflared.log")
    with log_path.open("w") as log:
        process = subprocess.Popen([
            cloudflared, "--no-autoupdate", "--config", str(connector_config),
            "tunnel", "--url", f"http://127.0.0.1:{port}", "--http-host-header", internal_host,
        ], stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + timeout
        public_url = None
        last_error = "no public URL received"
        observed = {path: set() for path in expected_paths}

        def public_request(path, extra=()):
            remaining = deadline - time.monotonic()
            return subprocess.run([
                curl, "--silent", "--show-error", "--fail",
                "--max-time", str(max(.01, min(5.0, remaining))),
                *(["--doh-url", doh_url] if doh_url else []), *extra, public_url + path,
            ], capture_output=True, text=True, timeout=max(.01, remaining))

        try:
            while time.monotonic() < deadline:
                assert process.poll() is None, f"cloudflared exited early: {process.returncode}"
                if public_url is None:
                    match = QUICK_URL.search(log_path.read_text(errors="replace"))
                    if match:
                        public_url = match.group(1)
                if public_url:
                    try:
                        for path, names in expected_paths.items():
                            for _ in names:
                                result = public_request(path)
                                if result.returncode == 0 and result.stdout in {name + ":" + path for name in names}:
                                    observed[path].add(result.stdout.split(":", 1)[0])
                                else:
                                    last_error = f"path={path}; curl exit={result.returncode}; response={result.stdout[:200]!r}; error={result.stderr[:500]!r}"
                        if all(observed[path] == names for path, names in expected_paths.items()):
                            origin = public_request("/headers")
                            if origin.returncode != 0:
                                last_error = f"public headers request failed: {origin.stderr[:500]!r}"
                            else:
                                headers = json.loads(origin.stdout)
                                public_host = urlsplit(public_url).hostname
                                assert headers["host"] == public_host and headers["forwardedProto"] == "https", headers
                                redirect = public_request("/redirect", ["--output", os.devnull, "--write-out", "%{http_code}\n%{redirect_url}"])
                                if redirect.returncode != 0:
                                    last_error = f"public redirect request failed: {redirect.stderr[:500]!r}"
                                else:
                                    assert redirect.stdout.splitlines() == ["302", public_url + "/landing"], redirect.stdout
                                    summary = {path: sorted(names) for path, names in observed.items()}
                                    print(f"PASS anonymous public HTTPS target={internal_host} paths={summary}: {public_url}", flush=True)
                                    print("PASS public Quick Tunnel preserves the actual hostname and HTTPS absolute redirects", flush=True)
                                    return public_url
                    except subprocess.TimeoutExpired:
                        last_error = "public HTTPS request timed out during DNS or connection setup"
                time.sleep(min(.5, max(0, deadline - time.monotonic())))
            summary = {path: sorted(names) for path, names in observed.items()}
            raise AssertionError(f"public Quick Tunnel QA exceeded {timeout}s: {last_error}; observed={summary}")
        except BaseException:
            print("CLOUDFLARED LOG (isolated test connector):\n" + log_path.read_text(errors="replace")[-8000:], flush=True)
            raise
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            print(f"CLEANUP isolated cloudflared stopped: exit={process.returncode}", flush=True)


def run(firecrab, nginx, cloudflared=None, cloudflared_timeout=60, doh_url=None):
    import signal
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="fc-pipe-", dir="/tmp")))
        backends = [Backend(name) for name in ["one", "two", "three"]]
        for backend in backends: stack.callback(backend.close)
        one, two, three = backends
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0)); port = reserved.getsockname()[1]
        manifest = {"listen": f"127.0.0.1:{port}", "routes": [
            {"host": "app.example.com", "upstreams": [one.address, two.address]},
            {"host": "app.example.com", "pathPrefix": "/api", "upstreams": [three.address]},
            {"host": "other.example.com", "upstreams": [three.address]}]}
        manifest_file = directory / "pipe.json"
        config_file = directory / "ingress.conf"
        def render():
            manifest_file.write_text(json.dumps(manifest))
            generated = subprocess.check_output([firecrab, "pipe", "render", "--config", str(manifest_file)], text=True)
            config_file.write_text(generated.replace("/run/firecrab-ingress/status.sock", str(directory / "status.sock")))
        def check():
            return subprocess.run([nginx, "-t", "-p", str(directory)+"/", "-c", str(config_file)], capture_output=True)
        render(); assert check().returncode == 0, check().stderr
        log = stack.enter_context((directory / "nginx.log").open("w+"))
        process = subprocess.Popen([nginx, "-p", str(directory)+"/", "-c", str(config_file), "-g", "daemon off;"], stdout=log, stderr=log)
        def stop():
            if process.poll() is None:
                process.send_signal(signal.SIGQUIT)
                try: process.wait(timeout=10)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
        stack.callback(stop)
        def wait(body=None, host="app.example.com", path="/"):
            for _ in range(100):
                try:
                    response = request(port, path, host=host)
                    if response[0] == 200 and (body is None or response[1].startswith(body)): return
                except (OSError, http.client.HTTPException): pass
                time.sleep(.05)
            log.seek(0); raise AssertionError(log.read())
        wait()
        try:
            bodies = {request(port)[1].split(":")[0] for _ in range(10)}
            assert bodies == {"one", "two"}, bodies
            for path in ["/api", "/api/users"]:
                assert request(port,path) == (200, "three:"+path)
            assert request(port,"/apiculture")[1].split(":")[0] in ["one", "two"]
            assert request(port,host="other.example.com")[1].startswith("three:")
            assert request(port,host="unknown.example.com")[0] == 404
            print("PASS domain/path boundaries and round-robin load balancing", flush=True)
            one.healthy = False
            for _ in range(12): assert request(port)[1].startswith("two:")
            assert one.failures == 1, one.failures
            print("PASS passive health excludes a failing app without the API", flush=True)
            two.slow_started.clear()
            with concurrent.futures.ThreadPoolExecutor() as executor:
                pending = executor.submit(request,port,"/slow")
                assert two.slow_started.wait(5)
                manifest["routes"][0]["upstreams"] = [three.address]
                render(); assert check().returncode == 0
                process.send_signal(signal.SIGHUP)
                assert pending.result() == (200,"two:/slow")
            wait("three:"); assert process.poll() is None
            print("PASS graceful reload keeps the master and in-flight request", flush=True)
            one.healthy = True
            public_domain = "my-app.firecrab.dev"
            manifest["routes"].extend([
                {"host": public_domain, "upstreams": [one.address, two.address]},
                {"host": public_domain, "pathPrefix": "/api", "upstreams": [three.address]},
            ])
            render(); assert check().returncode == 0, check().stderr
            process.send_signal(signal.SIGHUP)
            wait(host=public_domain)
            bodies = {request(port, host=public_domain)[1].split(":")[0] for _ in range(12)}
            assert bodies == {"one", "two"}, bodies
            assert request(port, "/api", host=public_domain) == (200, "three:/api")
            print("PASS existing public hostname reaches Nginx domain/path routes and both app targets without a registration service", flush=True)
            manifest["quickTunnel"] = {"upstreams": [one.address, two.address]}
            render(); assert check().returncode == 0
            process.send_signal(signal.SIGHUP)
            wait(host=QUICK_HOST)
            bodies = {request(port, host=QUICK_HOST)[1].split(":")[0] for _ in range(12)}
            assert bodies == {"one", "two"}, bodies
            for path in ["/api", "/api/users"]:
                status, body = request(port, path, host=QUICK_HOST)
                assert status == 200 and body in {"one:" + path, "two:" + path}, (status, body)
            assert request(port) == (200, "three:/")
            assert request(port, "/api") == (200, "three:/api")
            assert request(port, host="other.example.com") == (200, "three:/")
            assert request(port, host="random.trycloudflare.com")[0] == 404
            print("PASS Quick Tunnel internal Host distributes to both apps and preserves existing routes", flush=True)
            public_host = "qa-public.trycloudflare.com"
            forwarded = {"X-Forwarded-Host": public_host, "X-Forwarded-Proto": "https"}
            status, body = request(port, "/headers", host=QUICK_HOST, headers=forwarded)
            assert status == 200, (status, body)
            origin = json.loads(body)
            assert origin["host"] == public_host and origin["forwardedProto"] == "https", origin
            assert redirect_location(port, QUICK_HOST, forwarded) == "https://" + public_host + "/landing"
            status, body = request(port, "/headers", headers=forwarded)
            assert status == 200, (status, body)
            origin = json.loads(body)
            assert origin["host"] == "app.example.com", origin
            assert redirect_location(port, "app.example.com", forwarded) == "https://app.example.com/landing"
            print("PASS Quick Tunnel restores public Host and HTTPS redirects without overriding ordinary route Host", flush=True)
            one.slow_started.clear(); two.slow_started.clear()
            with concurrent.futures.ThreadPoolExecutor() as executor:
                pending = executor.submit(request, port, "/slow", QUICK_HOST)
                deadline = time.monotonic() + 5
                while not (one.slow_started.is_set() or two.slow_started.is_set()) and time.monotonic() < deadline:
                    time.sleep(.01)
                assert one.slow_started.is_set() or two.slow_started.is_set(), "Quick Tunnel slow request did not reach either app"
                manifest["quickTunnel"]["upstreams"] = [three.address]
                render(); assert check().returncode == 0
                process.send_signal(signal.SIGHUP)
                assert pending.result() in [(200, "one:/slow"), (200, "two:/slow")]
            wait("three:", host=QUICK_HOST)
            assert process.poll() is None
            assert request(port) == (200, "three:/")
            assert request(port, "/api") == (200, "three:/api")
            assert request(port, host="other.example.com") == (200, "three:/")
            print("PASS Quick Tunnel target reload preserves its in-flight request and existing routes", flush=True)
            manifest["routes"][0]["upstreams"] = [one.address, two.address]
            manifest["routes"].append({"host": "api-only.example.com", "pathPrefix": "/api", "upstreams": [three.address]})
            manifest["quickTunnels"] = [
                {"id": APP_QUICK_ID, "name": "Public web", "target": {"type": "apps", "upstreams": [one.address, two.address]}},
                {"id": ROUTES_QUICK_ID, "name": "Routed website", "target": {"type": "routes", "host": "app.example.com"}},
                {"id": SECOND_APP_QUICK_ID, "name": "Public API", "target": {"type": "apps", "upstreams": [three.address]}},
                {"id": API_ONLY_QUICK_ID, "name": "API paths only", "target": {"type": "routes", "host": "api-only.example.com"}},
            ]
            render(); assert check().returncode == 0, check().stderr
            process.send_signal(signal.SIGHUP)
            wait(host=quick_host(APP_QUICK_ID))
            for identifier in [APP_QUICK_ID, ROUTES_QUICK_ID]:
                bodies = {request(port, host=quick_host(identifier))[1].split(":")[0] for _ in range(12)}
                assert bodies == {"one", "two"}, (identifier, bodies)
            for path in ["/api", "/api/users"]:
                assert request(port, path, host=quick_host(APP_QUICK_ID))[1] in {"one:" + path, "two:" + path}
                assert request(port, path, host=quick_host(ROUTES_QUICK_ID)) == (200, "three:" + path)
                assert request(port, path, host=quick_host(SECOND_APP_QUICK_ID)) == (200, "three:" + path)
                assert request(port, path, host=quick_host(API_ONLY_QUICK_ID)) == (200, "three:" + path)
            assert request(port, "/apiculture", host=quick_host(ROUTES_QUICK_ID))[1] in {"one:/apiculture", "two:/apiculture"}
            assert request(port, host=quick_host(API_ONLY_QUICK_ID))[0] == 404
            assert request(port, "/apiculture", host=quick_host(API_ONLY_QUICK_ID))[0] == 404
            assert request(port, host=QUICK_HOST) == (200, "three:/")
            assert request(port, "/api") == (200, "three:/api")
            assert request(port, host="other.example.com") == (200, "three:/")
            print("PASS independent Quick hosts expose app pools or every saved domain path; API-only domains keep root and prefix boundaries", flush=True)
            for identifier in [APP_QUICK_ID, ROUTES_QUICK_ID, SECOND_APP_QUICK_ID, API_ONLY_QUICK_ID]:
                status, body = request(port, "/api/headers" if identifier == API_ONLY_QUICK_ID else "/headers", host=quick_host(identifier), headers=forwarded)
                assert status == 200, (status, body)
                origin = json.loads(body)
                assert origin["host"] == public_host and origin["forwardedProto"] == "https", origin
                if identifier != API_ONLY_QUICK_ID:
                    assert redirect_location(port, quick_host(identifier), forwarded) == "https://" + public_host + "/landing"
            if cloudflared:
                for identifier, expected_paths in [
                    (APP_QUICK_ID, {"/": {"one", "two"}, "/api": {"one", "two"}}),
                    (ROUTES_QUICK_ID, {"/": {"one", "two"}, "/api": {"three"}}),
                ]:
                    live_quick_tunnel(cloudflared, port, directory, cloudflared_timeout, doh_url,
                                      internal_host=quick_host(identifier), expected_paths=expected_paths)
            manifest["routes"][1]["upstreams"] = [two.address]
            render(); assert check().returncode == 0
            process.send_signal(signal.SIGHUP)
            wait("two:", host=quick_host(ROUTES_QUICK_ID), path="/api")
            assert request(port, "/api") == (200, "two:/api")
            assert request(port, "/api/users", host=quick_host(ROUTES_QUICK_ID)) == (200, "two:/api/users")
            print("PASS saved route updates reach the same Quick domain group without editing its target", flush=True)
            missing_route = json.loads(json.dumps(manifest))
            missing_route["routes"] = [route for route in missing_route["routes"] if route["host"] != "app.example.com"]
            missing_file = directory / "missing-route.json"; missing_file.write_text(json.dumps(missing_route))
            rejected = subprocess.run([firecrab, "pipe", "render", "--config", str(missing_file)], capture_output=True)
            assert rejected.returncode != 0, "referenced route domain was removed without rejecting its Quick Tunnel target"
            assert request(port, "/api", host=quick_host(ROUTES_QUICK_ID)) == (200, "two:/api")
            one.slow_started.clear(); two.slow_started.clear()
            with concurrent.futures.ThreadPoolExecutor() as executor:
                pending = executor.submit(request, port, "/slow", quick_host(APP_QUICK_ID))
                deadline = time.monotonic() + 5
                while not (one.slow_started.is_set() or two.slow_started.is_set()) and time.monotonic() < deadline:
                    time.sleep(.01)
                assert one.slow_started.is_set() or two.slow_started.is_set(), "multi-tunnel request did not reach its own app pool"
                manifest["quickTunnels"][0]["target"]["upstreams"] = [three.address]
                manifest["quickTunnels"] = [entry for entry in manifest["quickTunnels"] if entry["id"] != SECOND_APP_QUICK_ID]
                render(); assert check().returncode == 0
                process.send_signal(signal.SIGHUP)
                assert pending.result() in [(200, "one:/slow"), (200, "two:/slow")]
            wait("three:", host=quick_host(APP_QUICK_ID))
            assert request(port, host=quick_host(SECOND_APP_QUICK_ID))[0] == 404
            assert request(port, "/api", host=quick_host(ROUTES_QUICK_ID)) == (200, "two:/api")
            assert request(port, "/api", host=quick_host(API_ONLY_QUICK_ID)) == (200, "three:/api")
            assert request(port, host=QUICK_HOST) == (200, "three:/")
            print("PASS editing and deleting one Quick target preserves other tunnels, routes and in-flight requests", flush=True)
            previous = config_file.read_text(); config_file.write_text("invalid configuration;")
            assert check().returncode != 0
            process.send_signal(signal.SIGHUP); time.sleep(.2)
            assert request(port)[1].split(":")[0] in {"one", "two"}
            assert request(port, host=QUICK_HOST)[1].startswith("three:")
            assert request(port, host=quick_host(APP_QUICK_ID))[1].startswith("three:")
            assert request(port, "/api", host=quick_host(ROUTES_QUICK_ID)) == (200, "two:/api")
            config_file.write_text(previous)
            print("PASS rejected Nginx config keeps the previous route serving", flush=True)
        except BaseException:
            log.seek(0); print(log.read()); raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecrab", required=True)
    parser.add_argument("--nginx", required=True)
    parser.add_argument("--cloudflared", help="Optional cloudflared binary for isolated public HTTPS QA; omitted by default")
    parser.add_argument("--cloudflared-timeout", type=float, default=60, help="Total public URL and HTTPS verification deadline in seconds (1-60; default: 60)")
    parser.add_argument("--doh-url", help="Optional DNS-over-HTTPS resolver URL for isolated public curl probes only; does not change host or guest DNS")
    args = parser.parse_args()
    if not 1 <= args.cloudflared_timeout <= 60:
        parser.error("--cloudflared-timeout must be between 1 and 60 seconds")
    run(args.firecrab, args.nginx, args.cloudflared, args.cloudflared_timeout, args.doh_url)
