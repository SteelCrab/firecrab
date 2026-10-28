#!/usr/bin/env python3
"""Exercise generated internal routes with real Nginx, DNS and two HTTP backends.

Run as root in a disposable Linux Docker container with NET_ADMIN. This is
transport coverage; it does not boot Firecracker or replace real MicroVM QA.
"""

import argparse
import base64
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import threading
import time
import uuid


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT)


def dns_query(name):
    identifier = 335
    question = b"".join(bytes([len(part)]) + part.encode() for part in name.split(".")) + b"\0\0\1\0\1"
    packet = struct.pack("!6H", identifier, 0x100, 1, 0, 0, 0) + question
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
        client.bind(("172.30.0.10", 0))
        client.settimeout(2)
        client.sendto(packet, ("172.30.0.1", 1053))
        reply = client.recv(4096)
    header = struct.unpack("!6H", reply[:12])
    assert header[0] == identifier
    return header[1] & 15, header[3], reply


def get(host="api.internal", source="172.30.0.10", port=80, target="172.30.0.1"):
    connection = http.client.HTTPConnection(target, port, timeout=3, source_address=(source, 0))
    try:
        connection.request("GET", "/hello?value=335", headers={"Host": host, "X-Forwarded-Proto": "https"})
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


class App(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.headers.get("Upgrade", "").lower() == "websocket":
            key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
            self.end_headers()
            frame = self.rfile.read(2)
            mask = self.rfile.read(4)
            payload = self.rfile.read(frame[1] & 127)
            payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
            self.wfile.write(bytes([0x81, len(payload)]) + payload)
            return
        body = json.dumps({"replica": self.server.server_address[0], "path": self.path,
                           "host": self.headers["Host"], "proto": self.headers["X-Forwarded-Proto"]}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def wait_ready(probe):
    deadline = time.monotonic() + 10
    while True:
        try:
            if probe():
                return
        except (OSError, AssertionError):
            pass
        if time.monotonic() >= deadline:
            raise AssertionError("transport readiness timed out")
        time.sleep(0.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--nginx", type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or not Path("/.dockerenv").is_file():
        parser.error("use a disposable Linux Docker container as root with NET_ADMIN")
    cli, nginx = args.cli.resolve(), args.nginx.resolve()
    interface = "qa335" + uuid.uuid4().hex[:6]
    children, apps = [], []
    status = Path("/run/firecrab-ingress")
    if status.exists():
        parser.error("refusing to reuse an existing ingress runtime directory")
    run("ip", "link", "add", interface, "type", "dummy")
    try:
        run("ip", "link", "set", interface, "up")
        for address in ["172.30.0.1/24", "172.30.0.2/24", "172.30.0.3/24", "172.30.0.10/24", "172.30.0.11/24", "172.31.0.10/24"]:
            run("ip", "address", "add", address, "dev", interface)
        status.mkdir()
        with tempfile.TemporaryDirectory(prefix="qa335-internal-") as directory:
            root = Path(directory)
            manifest, configuration, hosts = [root / name for name in ["pipe.json", "nginx.conf", "services.hosts"]]
            owner = {"serviceId": str(uuid.uuid4()), "deploymentId": str(uuid.uuid4())}
            private = {"host": "api.internal", "pathPrefix": "/", "owner": owner,
                       "network": {"micro_network_id": str(uuid.uuid4()), "gateway": "172.30.0.1", "prefix": 24},
                       "upstreams": ["172.30.0.2:18080", "172.30.0.3:18080"]}
            public = {"host": "public.example.com", "upstreams": ["172.30.0.3:18080"]}

            def render(routes):
                manifest.write_text(json.dumps({"listen": "127.0.0.1:18081", "routes": routes, "tunnel": None}))
                configuration.write_text(run(str(cli), "pipe", "render", "--config", str(manifest)))
                run(str(nginx), "-t", "-p", directory + "/", "-c", str(configuration))

            render([private, public])
            hosts.write_text("172.30.0.1 api.internal\n")
            log = (root / "transport.log").open("w+")
            for address in ["172.30.0.2", "172.30.0.3"]:
                app = http.server.ThreadingHTTPServer((address, 18080), App)
                app.daemon_threads = True
                apps.append(app)
                threading.Thread(target=app.serve_forever, daemon=True).start()
            dns = subprocess.Popen(["dnsmasq", "--no-daemon", "--no-resolv", "--no-hosts", "--port=1053",
                                    "--bind-interfaces", "--interface=" + interface, "--local=/internal/",
                                    "--addn-hosts=" + str(hosts)], stdout=log, stderr=log)
            children.append(dns)
            ingress = subprocess.Popen([str(nginx), "-p", directory + "/", "-c", str(configuration), "-g", "daemon off;"], stdout=log, stderr=log)
            children.append(ingress)
            wait_ready(lambda: get()[0] == 200 and dns_query("api.internal")[:2] == (0, 1))
            assert dns_query("unknown.internal")[:2] == (3, 0)
            assert socket.inet_aton("172.30.0.1") in dns_query("api.internal")[2]
            print("PASS: internal A record resolves; unknown internal name returns NXDOMAIN", flush=True)
            responses = []
            for _ in range(20):
                code, body = get()
                assert code == 200
                responses.append(json.loads(body))
            assert {response["replica"] for response in responses} == {"172.30.0.2", "172.30.0.3"}
            assert all(response["path"] == "/hello?value=335" and response["host"] == "api.internal"
                       and response["proto"] == "http" for response in responses)
            assert get(source="172.31.0.10")[0] == 403
            assert get(host="unknown.internal")[0] == 404
            assert get(target="127.0.0.1", port=18081)[0] == 404
            print("PASS: both replicas receive HTTP; paths/Host survive; foreign and public listeners reject private access", flush=True)
            result = run("curl", "--noproxy", "*", "--fail", "--silent", "--max-time", "5", "--http2-prior-knowledge",
                         "--interface", "172.30.0.10", "--resolve", "api.internal:80:172.30.0.1", "http://api.internal/h2")
            assert json.loads(result)["path"] == "/h2"
            print("PASS: HTTP/2 prior knowledge reaches the application", flush=True)
            with socket.create_connection(("172.30.0.1", 80), timeout=3, source_address=("172.30.0.10", 0)) as connection:
                connection.sendall(b"GET /ws HTTP/1.1\r\nHost: api.internal\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
                reply = b""
                while not reply.endswith(b"\r\n\r\n"):
                    reply += connection.recv(1)
                assert b"101 Switching Protocols" in reply
                connection.sendall(b"\x81\x85mask" + bytes(value ^ b"mask"[index % 4] for index, value in enumerate(b"hello")))
                stream = connection.makefile("rb")
                assert stream.read(7) == b"\x81\x05hello"
            print("PASS: WebSocket upgrade and bidirectional messages", flush=True)
            private["securityGroup"] = {"id": str(uuid.uuid4()), "micro_network_id": private["network"]["micro_network_id"],
                                        "rules": [{"direction": "ingress", "protocol": "tcp", "port_from": 18080,
                                                   "port_to": 18080, "peer_address": "172.30.0.10", "peer_prefix": 32}]}
            render([private, public])
            ingress.send_signal(signal.SIGHUP)
            wait_ready(lambda: get()[0] == 200 and get(source="172.30.0.11")[0] == 403)
            private["securityGroup"]["rules"] = []
            render([private, public])
            ingress.send_signal(signal.SIGHUP)
            wait_ready(lambda: get()[0] == 403)
            del private["securityGroup"]
            render([private, public])
            ingress.send_signal(signal.SIGHUP)
            wait_ready(lambda: get()[0] == 200)
            print("PASS: ingress policy permits only its source peer and an empty group denies access", flush=True)
            apps[0].shutdown()
            apps[0].server_close()
            assert all(get()[0] == 200 for _ in range(8))
            print("PASS: a failed replica is removed through passive failover", flush=True)
            render([public])
            ingress.send_signal(signal.SIGHUP)
            hosts.write_text("")
            dns.send_signal(signal.SIGHUP)
            wait_ready(lambda: dns_query("api.internal")[:2] == (3, 0))
            wait_ready(lambda: get(host="public.example.com", target="127.0.0.1", port=18081)[0] == 200)
            print("PASS: withdrawal removes DNS and preserves the unrelated public route", flush=True)
    finally:
        for child in reversed(children):
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        for app in apps:
            app.shutdown()
            app.server_close()
        run("ip", "link", "delete", interface)
        if status.exists():
            for entry in status.iterdir():
                entry.unlink()
            status.rmdir()
    print("PASS: temporary processes, interface and runtime paths removed", flush=True)


if __name__ == "__main__":
    main()
