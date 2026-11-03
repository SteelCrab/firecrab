#!/usr/bin/env python3
"""Check Deploy's generated data plane with real Nginx and local HTTP apps.

This verifies reload/drain behavior. It does not replace MicroVM/KVM acceptance.
"""

import argparse
import base64
import concurrent.futures
import contextlib
import hashlib
import http.client
import http.server
import json
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time


def receive(stream, count):
    result = b""
    while len(result) < count:
        part = stream.recv(count - len(result))
        if not part:
            raise EOFError("connection closed during drain")
        result += part
    return result


class App:
    def __init__(self, name):
        self.name = name
        self.started = threading.Event()
        self.release = threading.Event()
        self.requests = []
        app = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self):
                self.rfile.read(int(self.headers.get("Content-Length", 0)))
                self.do_GET()

            def do_GET(self):
                app.requests.append((self.command, self.path))
                if self.path == "/late-exit" and app.name == "faulty":
                    app.started.set()
                    assert app.release.wait(20), "fault injection was not released"
                    self.close_connection = True
                    return
                if self.path == "/ws":
                    key = self.headers["Sec-WebSocket-Key"]
                    accept = base64.b64encode(hashlib.sha1(
                        (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                    ).digest()).decode()
                    self.send_response(101)
                    self.send_header("Upgrade", "websocket")
                    self.send_header("Connection", "Upgrade")
                    self.send_header("Sec-WebSocket-Accept", accept)
                    self.end_headers()
                    try:
                        while True:
                            header = receive(self.connection, 2)
                            if header[0] & 15 == 8:
                                break
                            size = header[1] & 127
                            assert size < 126 and header[1] & 128
                            mask = receive(self.connection, 4)
                            payload = receive(self.connection, size)
                            decoded = bytes(value ^ mask[i % 4] for i, value in enumerate(payload))
                            reply = app.name.encode() + b":" + decoded
                            self.connection.sendall(bytes([0x81, len(reply)]) + reply)
                    except (OSError, EOFError):
                        pass
                    self.close_connection = True
                    return
                if self.path == "/held":
                    app.started.set()
                    assert app.release.wait(20), "held request was not released"
                body = (app.name + ":" + self.path).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
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
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def request(port, host="deploy.example.com", path="/", method="GET"):
    with contextlib.closing(http.client.HTTPConnection("127.0.0.1", port, timeout=10)) as conn:
        conn.request(method, path, headers={"Host": host})
        response = conn.getresponse()
        return response.status, response.read().decode()


def websocket(port):
    stream = socket.create_connection(("127.0.0.1", port), timeout=10)
    stream.sendall(b"GET /ws HTTP/1.1\r\nHost: deploy.example.com\r\n"
                   b"Connection: Upgrade\r\nUpgrade: websocket\r\n"
                   b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
                   b"Sec-WebSocket-Version: 13\r\n\r\n")
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        header += receive(stream, 1)
    assert header.startswith(b"HTTP/1.1 101"), header
    return stream


def echo(stream, text):
    payload = text.encode()
    mask = b"test"
    stream.sendall(bytes([0x81, 128 | len(payload)]) + mask
                   + bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))
    header = receive(stream, 2)
    assert header[0] == 0x81 and header[1] < 126, header
    return receive(stream, header[1]).decode()


def run(firecrab, nginx):
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="fc-deploy-", dir="/tmp")))
        old, new, other, faulty = [App(name) for name in ["old", "new", "other", "faulty"]]
        for app in [old, new, other, faulty]:
            stack.callback(app.close)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        manifest = {"listen": f"127.0.0.1:{port}", "routes": [
            {"host": "deploy.example.com", "upstreams": [old.address]},
            {"host": "other.example.com", "upstreams": [other.address]}]}
        config = directory / "nginx.conf"
        manifest_file = directory / "pipe.json"
        status_socket = directory / "status.sock"

        def render(upstreams):
            manifest["routes"][0]["upstreams"] = upstreams
            manifest_file.write_text(json.dumps(manifest))
            text = subprocess.check_output([firecrab, "pipe", "render", "--config", str(manifest_file)], text=True)
            assert "worker_shutdown_timeout 0;" in text
            if sys.platform == "darwin":
                # Native ARM macOS uses 16 KiB pages. Linux production uses 4 KiB.
                text = re.sub(r"(zone fc_route_\d+) 64k;", r"\1 256k;", text)
            config.write_text(text.replace("/run/firecrab-ingress/status.sock", str(status_socket)))
            return re.search(r'return 200 "([a-f0-9]+)"', text)[1]

        def check():
            return subprocess.run([nginx, "-t", "-p", str(directory) + "/", "-c", str(config)], capture_output=True)

        def wait_generation(expected):
            for _ in range(100):
                try:
                    with socket.socket(socket.AF_UNIX) as conn:
                        conn.settimeout(1)
                        conn.connect(str(status_socket))
                        conn.sendall(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                        response = b""
                        while part := conn.recv(4096):
                            response += part
                        if response.partition(b"\r\n\r\n")[2].decode() == expected:
                            return
                except OSError:
                    pass
                time.sleep(.05)
            raise AssertionError("Nginx did not acknowledge the route generation")

        generation = render([old.address])
        result = check()
        assert result.returncode == 0, result.stderr
        log = stack.enter_context((directory / "nginx.log").open("w+"))
        process = subprocess.Popen([nginx, "-p", str(directory) + "/", "-c", str(config), "-g", "daemon off;"], stdout=log, stderr=log)

        def stop():
            if process.poll() is None:
                process.send_signal(signal.SIGQUIT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()

        stack.callback(stop)
        wait_generation(generation)
        assert request(port) == (200, "old:/")
        connection = stack.enter_context(websocket(port))
        assert echo(connection, "before") == "old:before"
        errors, samples = [], []
        done = threading.Event()

        def load():
            while not done.is_set():
                try:
                    response = request(port)
                    samples.append(response)
                    if response not in [(200, "old:/"), (200, "new:/")]:
                        errors.append(response)
                except Exception as error:
                    errors.append(str(error))
                time.sleep(.005)

        worker = threading.Thread(target=load, daemon=True)
        worker.start()
        try:
            with concurrent.futures.ThreadPoolExecutor() as executor:
                held = executor.submit(request, port, "deploy.example.com", "/held")
                assert old.started.wait(5)
                for upstreams, expected in [([old.address, new.address], {"old:/", "new:/"}),
                                            ([new.address], {"new:/"}),
                                            ([old.address], {"old:/"})]:
                    generation = render(upstreams)
                    result = check()
                    assert result.returncode == 0, result.stderr
                    process.send_signal(signal.SIGHUP)
                    wait_generation(generation)
                    # Generation acknowledgement can precede old workers closing
                    # their listening sockets. Deploy also waits for worker drain.
                    deadline = time.monotonic() + 5
                    observed = set()
                    while time.monotonic() < deadline:
                        observed = {request(port)[1] for _ in range(40)}
                        if observed == expected:
                            break
                        time.sleep(.05)
                    assert observed == expected, (upstreams, observed)
                    assert request(port, "other.example.com") == (200, "other:/")
                    assert echo(connection, "after") == "old:after"
                    assert process.poll() is None
                # An invalid candidate never replaces the saved, serving config.
                valid = config.read_text()
                config.write_text(valid + "invalid_deploy_directive;\n")
                assert check().returncode != 0
                config.write_text(valid)
                assert request(port) == (200, "old:/")
                old.release.set()
                assert held.result(timeout=10) == (200, "old:/held")
        finally:
            old.release.set()
            done.set()
            worker.join(timeout=10)
        assert len(samples) >= 10, len(samples)
        assert not errors, errors
        print(f"PASS Deploy rolling switch and rollback: {len(samples)} continuous requests, zero errors")
        print("PASS held HTTP and WebSocket survive reloads; unrelated route and invalid-config recovery remain intact")
        # A VM can exit after a valid request has already waited for its response.
        # Each fresh route starts with the faulty peer, then the healthy peer.
        for method in ["GET", "POST"]:
            host = method.lower() + ".failover.example.test"
            faulty.started.clear()
            faulty.release.clear()
            manifest["routes"].append({"host": host, "upstreams": [faulty.address, new.address]})
            generation = render([old.address])
            assert check().returncode == 0
            process.send_signal(signal.SIGHUP)
            wait_generation(generation)
            before = new.requests.count((method, "/late-exit"))
            with concurrent.futures.ThreadPoolExecutor() as executor:
                pending = executor.submit(request, port, host, "/late-exit", method)
                try:
                    assert faulty.started.wait(5), "request did not reach the faulty peer"
                    time.sleep(4)  # Exceed the previous three-second retry cutoff.
                finally:
                    faulty.release.set()
                result = pending.result(timeout=5)
            if method == "GET":
                assert result == (200, "new:/late-exit"), ("late GET did not fail over", result)
                assert new.requests.count((method, "/late-exit")) == before + 1
            else:
                assert result[0] == 502, ("a sent mutation must not be replayed", result)
                assert new.requests.count((method, "/late-exit")) == before
            assert request(port, "other.example.com") == (200, "other:/")
        print("PASS late upstream exit retries GET on a healthy peer and never replays POST")



if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecrab", required=True)
    parser.add_argument("--nginx", required=True)
    args = parser.parse_args()
    run(str(Path(args.firecrab).resolve()), str(Path(args.nginx).resolve()))
