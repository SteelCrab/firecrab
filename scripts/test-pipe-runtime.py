#!/usr/bin/env python3
"""Exercise generated PipeIngress config against real Nginx, without root."""

import argparse
import concurrent.futures
import contextlib
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time


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
                body = f"{backend.name}:{self.path}".encode()
                self.send_response(status)
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


def request(port, path="/", host="app.example.com"):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
    try:
        connection.request("GET", path, headers={"Host": host})
        response = connection.getresponse()
        return response.status, response.read().decode()
    finally:
        connection.close()


def run(firecrab, nginx):
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
        def wait(body=None):
            for _ in range(100):
                try:
                    response = request(port)
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
            previous = config_file.read_text(); config_file.write_text("invalid configuration;")
            assert check().returncode != 0
            process.send_signal(signal.SIGHUP); time.sleep(.2)
            assert request(port)[1].startswith("three:")
            config_file.write_text(previous)
            print("PASS rejected Nginx config keeps the previous route serving", flush=True)
        except BaseException:
            log.seek(0); print(log.read()); raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecrab", required=True)
    parser.add_argument("--nginx", required=True)
    args = parser.parse_args()
    run(args.firecrab, args.nginx)
