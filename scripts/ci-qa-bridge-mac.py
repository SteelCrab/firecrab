#!/usr/bin/env python3
"""Check gateway MAC stability with the real helper in a private Linux netns."""

import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import uuid


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path, required=True)
    args = parser.parse_args()
    assert os.geteuid() == 0, "requires root and Linux network namespaces"
    helper = args.helper.resolve(strict=True)
    namespace = "fc480-" + uuid.uuid4().hex[:10]
    run("ip", "netns", "add", namespace)
    child = None
    try:
        with tempfile.TemporaryDirectory(prefix="fc480-") as directory:
            socket_path = str(Path(directory) / "helper.sock")
            log = (Path(directory) / "helper.log").open("w+")

            def start():
                nonlocal child
                process = subprocess.Popen(
                    ["ip", "netns", "exec", namespace, str(helper)],
                    env={**os.environ, "FIRECRAB_NET_HELPER_SOCK": socket_path},
                    stdout=log,
                    stderr=subprocess.STDOUT,
                )
                child = process
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    assert process.poll() is None, "helper exited during startup"
                    try:
                        request({"operation": "ensure_bridge"})
                        return process
                    except (FileNotFoundError, ConnectionRefusedError):
                        time.sleep(0.05)
                raise AssertionError("helper did not listen")

            def request(body):
                data = json.dumps({"version": 3, "request_id": str(uuid.uuid4()), "request": body}).encode()
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(10)
                    client.connect(socket_path)
                    client.sendall(struct.pack("!I", len(data)) + data)

                    def exact(length):
                        data = b""
                        while len(data) < length:
                            chunk = client.recv(length - len(data))
                            assert chunk, "helper closed the response"
                            data += chunk
                        return data

                    response = json.loads(exact(struct.unpack("!I", exact(4))[0]))
                    assert response["result"] == {"Ok": None}, response

            def ip(*args):
                return run("ip", "-n", namespace, *args)

            def mac(name):
                return json.loads(ip("-j", "link", "show", name))[0]["address"]

            def add_port(name, address, bridge):
                ip("link", "add", name, "type", "veth", "peer", "name", name + "p")
                ip("link", "set", name, "address", address, "master", bridge, "up")

            child = start()
            network_id = str(uuid.uuid4())
            ensure = {"operation": "ensure_micro_network_bridge", "micro_network_id": network_id,
                      "gateway": "172.29.48.1", "prefix": 24}
            request(ensure)
            bridges = json.loads(ip("-j", "link", "show", "type", "bridge"))
            bridge = next(link["ifname"] for link in bridges if link["ifname"] != "fcbr0")

            for name in [bridge, "fcbr0"]:
                initial = mac(name)
                add_port("high480", "0a:00:00:00:48:20", name)
                assert mac(name) == initial, "gateway MAC changed when a port joined: " + name
                add_port("low480", "02:00:00:00:48:10", name)
                assert mac(name) == initial, "gateway MAC followed the lowest port: " + name
                ip("link", "del", "low480")
                assert mac(name) == initial, "gateway MAC changed when a port left: " + name
                ip("link", "del", "high480")
                assert mac(name) == initial, "gateway MAC changed after the last port: " + name
            print("PASS new MicroNetwork and legacy gateways survive port churn", flush=True)

            ip("link", "del", bridge)
            ip("link", "add", bridge, "type", "bridge")
            add_port("high480", "0a:00:00:00:48:20", bridge)
            existing = mac(bridge)
            request(ensure)
            assert mac(bridge) == existing, "repair changed the existing gateway MAC"
            add_port("low480", "02:00:00:00:48:10", bridge)
            ip("link", "del", "high480")
            assert mac(bridge) == existing, "repaired gateway still follows its ports"
            print("PASS existing automatic gateway is pinned without changing its MAC", flush=True)

            child.terminate()
            child.wait(timeout=10)
            child = start()
            request(ensure)
            ip("link", "del", "low480")
            assert mac(bridge) == existing, "helper restart changed the pinned gateway"
            print("PASS helper restart retains the pinned gateway MAC", flush=True)
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        run("ip", "netns", "delete", namespace)


if __name__ == "__main__":
    main()
