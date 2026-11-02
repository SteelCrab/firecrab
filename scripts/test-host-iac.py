#!/usr/bin/env python3
"""Exercise the real privileged metadata broker inside a disposable Linux container.

This is a filesystem/protocol fixture. It does not run systemd, Nginx, a guest,
Terraform, or Stack. Native rejection checks must precede every service action.
"""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    if os.geteuid() != 0 or not Path("/.dockerenv").is_file():
        parser.error("run only as root inside a disposable Docker verification container")
    binary = args.binary.resolve(strict=True)
    Path("/run/systemd/system").mkdir(parents=True, exist_ok=True)
    machine_id = Path("/etc/machine-id")
    if not machine_id.exists():
        machine_id.write_text(uuid.uuid4().hex + "\n")
    endpoint = str(uuid.uuid4())
    with tempfile.TemporaryDirectory(prefix="fc-host-iac-") as temporary:
        root = Path(temporary)
        config_root = root / "conf"
        pipe = config_root / "pipe"
        pipe.mkdir(parents=True)
        unit_root = root / "units"
        unit_root.mkdir()
        fixture_bin = root / "bin"
        fixture_bin.mkdir()
        systemctl = fixture_bin / "systemctl"
        ready_script = "#!/bin/sh\ncase \"$1\" in\n show) printf 'ActiveState=active\\n';;\n list-jobs) exit 0;;\n *) exit 99;;\nesac\n"
        systemctl.write_text(ready_script)
        systemctl.chmod(0o755)
        quick_id, peer_id = str(uuid.uuid4()), str(uuid.uuid4())
        config = {"routes": [
            {"host": "app.example.com", "pathPrefix": "/", "upstreams": ["127.0.0.1:8081"]},
            {"host": "peer.example.com", "pathPrefix": "/", "upstreams": ["127.0.0.1:8082"]},
        ], "tunnel": None, "quickTunnel": {"upstreams": ["127.0.0.1:8085"]},
            "quickTunnels": [
                {"id": quick_id, "name": "fixture-app", "target": {"type": "apps", "upstreams": ["127.0.0.1:8083"]}},
                {"id": peer_id, "name": "fixture-peer", "target": {"type": "apps", "upstreams": ["127.0.0.1:8084"]}},
            ]}
        manifest = pipe / "manifest.json"
        original = json.dumps(config).encode()
        manifest.write_bytes(original)
        env = dict(os.environ, CONFDIR=str(config_root), UNITDIR=str(unit_root),
                   PATH=str(fixture_bin) + os.pathsep + os.environ["PATH"])

        def command(arguments, body=None, success=True):
            completed = subprocess.run([str(binary), "pipe", *arguments],
                                       input=None if body is None else json.dumps(body).encode(),
                                       capture_output=True, env=env, timeout=20)
            assert (completed.returncode == 0) == success, (arguments, completed.stderr.decode())
            return completed

        def rpc(body):
            return json.loads(command(["host-iac"], body).stdout)

        resource = {"kind": "ingress", "id": "route:app.example.com/"}

        def observe():
            return rpc({"action": "observe", "endpoint_id": endpoint, "resource": resource})["observation"]

        def change(owner, observation):
            ownership = observation["ownership"]
            return {"action": "change_owner", "request": {
                "change": {"operationId": str(uuid.uuid4()), "expectedEndpointId": endpoint,
                           "resource": resource, "expectedGeneration": ownership["generation"],
                           "expectedOwner": ownership["owner"], "owner": owner},
                "host": observation["host"],
                "configurationSha256": observation["configurationSha256"],
            }}

        def confirm(request):
            return rpc({"action": "confirm_owner", "endpoint_id": endpoint,
                        "operation_id": request["request"]["change"]["operationId"]})

        observed = observe()
        assert observed["exists"] and not observed["parentControlled"]
        owner = {"manager": "terraform", "id": str(uuid.uuid4()), "key": "firecrab_ingress.app"}
        systemctl.write_text(ready_script.replace("list-jobs) exit 0", "list-jobs) printf '12 firecrab-ingress.service stop waiting\\n'"))
        busy = observe()
        assert busy["busy"]
        assert rpc(change(owner, busy))["kind"] == "owner_rejected"
        systemctl.write_text(ready_script)
        assert not observe()["busy"]
        adopt = change(owner, observed)
        receipt = rpc(adopt)["receipt"]
        assert receipt["ownership"]["generation"] == 1 and not receipt["confirmed"]
        assert (pipe / "ownership.json").stat().st_mode & 0o777 == 0o600
        assert manifest.read_bytes() == original
        for operation, body in [(["disable"], None), (["control"], {"service": "ingress", "action": "stop"})]:
            denied = command(operation, body, success=False)
            assert b"managed resources" in denied.stderr
            assert manifest.read_bytes() == original
        candidate = root / "candidate.json"
        changed = json.loads(original)
        changed["routes"][0]["upstreams"] = ["127.0.0.1:9000"]
        candidate.write_text(json.dumps(changed))
        denied = command(["apply", "--config", str(candidate), "--nginx", "/bin/true",
                          "--cloudflared", "/bin/true"], success=False)
        assert b"Pipe resource is managed" in denied.stderr
        confirm(adopt)
        assert rpc(adopt)["receipt"]["ownership"] == receipt["ownership"]
        assert manifest.read_bytes() == original

        release = change(None, observe())
        assert rpc(release)["receipt"]["ownership"]["generation"] == 2
        assert b"unconfirmed ownership" in command(["disable"], success=False).stderr
        confirm(release)
        # Stop at fake engine validation: no systemd job or real engine is invoked.
        passed_gate = command(["apply", "--config", str(candidate), "--nginx", "/bin/false",
                               "--cloudflared", "/bin/true"], success=False)
        assert b"Pipe resource is managed" not in passed_gate.stderr
        assert b"-t" in passed_gate.stderr
        assert manifest.read_bytes() == original

        pipe.chmod(0o777)
        denied = command(["host-iac"], {"action": "observe", "endpoint_id": endpoint,
                                      "resource": resource}, success=False)
        assert b"protected and root-owned" in denied.stderr
        pipe.chmod(0o755)

        stale = change(owner, observe())
        stale["request"]["host"]["inode"] += 1
        rejected = rpc(stale)
        assert rejected["kind"] == "owner_rejected"
        stale["request"]["host"] = observe()["host"]
        assert rpc(stale) == rejected

        # A retained acknowledgement remains readable after later manifest damage.
        manifest.write_text("invalid fixture JSON")
        assert rpc(adopt)["receipt"]["ownership"] == receipt["ownership"]
        unavailable = command(["host-iac"], {"action": "observe", "endpoint_id": endpoint,
                                             "resource": resource}, success=False)
        assert unavailable.returncode != 0
        manifest.write_bytes(original)

        lock = Path("/run/firecrab-pipe-apply.lock").open("r+")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            denied = command(["host-iac"], change(None, observe_without_rpc(observed, 2)), success=False)
            assert b"another pipe change" in denied.stderr
        finally:
            lock.close()
        assert manifest.read_bytes() == original

        resource = {"kind": "quick_tunnel", "id": quick_id}
        quick_owner = dict(owner, key="firecrab_quick_tunnel.app")
        quick = change(quick_owner, observe())
        assert rpc(quick)["receipt"]["ownership"]["generation"] == 1
        denied = command(["quick-manage"], {"action": "control", "id": quick_id,
                                             "serviceAction": "stop"}, success=False)
        assert b"connector is managed" in denied.stderr
        changed = json.loads(original)
        changed["quickTunnels"][0]["target"]["upstreams"] = ["127.0.0.1:9090"]
        candidate.write_text(json.dumps(changed))
        denied = command(["apply", "--config", str(candidate), "--nginx", "/bin/true",
                          "--cloudflared", "/bin/true"], success=False)
        assert b"managed" in denied.stderr
        confirm(quick)
        released = change(None, observe())
        rpc(released)
        confirm(released)
        assert rpc(quick)["receipt"]["ownership"]["generation"] == 1

        resource = {"kind": "quick_tunnel", "id": str(uuid.UUID(int=1))}
        legacy = change(dict(owner, key="firecrab_quick_tunnel.legacy"), observe())
        rpc(legacy)
        confirm(legacy)
        denied = command(["control"], {"service": "tunnel", "action": "stop"}, success=False)
        assert b"connector is managed" in denied.stderr

        target = {"type": "apps", "upstreams": ["127.0.0.1:8084"]}
        parent = pipe / "firecrab-addresses.json"
        reservation = {"operationId": str(uuid.uuid4()), "effectsStarted": True,
                       "previousProtocol": "addresses_v1", "subdomain": "fixture-peer",
                       "target": target, "connectorId": peer_id, "token": "fixture-private-token",
                       "protocol": "addresses_v1"}
        private_store = {"endpoint": "https://registry.invalid", "protocol": "addresses_v1",
                         "bindings": [], "pending": [reservation]}
        parent.write_text(json.dumps(private_store))
        parent.chmod(0o600)
        resource = {"kind": "quick_tunnel", "id": peer_id}
        pending = observe()
        assert pending["parentControlled"] and pending["busy"]
        assert "fixture-private-token" not in json.dumps(pending)
        assert rpc(change(dict(owner, key="firecrab_quick_tunnel.peer"), pending))["kind"] == "owner_rejected"
        denied = command(["control"], {"service": "ingress", "action": "stop"}, success=False)
        assert b"original Pipe address operation" in denied.stderr
        private_store["pending"] = []
        private_store["bindings"] = [{"subdomain": "fixture-peer", "target": target,
                                      "connectorId": peer_id, "ownedConnector": True,
                                      "token": "fixture-private-token", "registered": True,
                                      "connected": True, "lastSuccess": 1, "error": None}]
        parent.write_text(json.dumps(private_store))
        controlled = observe()
        assert controlled["parentControlled"] and not controlled["busy"]
        assert rpc(change(dict(owner, key="firecrab_quick_tunnel.peer"), controlled))["kind"] == "owner_rejected"
        assert manifest.read_bytes() == original
        retained = json.loads((pipe / "ownership.json").read_text())
        assert retained["schemaVersion"] == 1
        assert "quick_tunnel:" + quick_id in retained["owners"]
        print(json.dumps({"kind": "privileged-host-broker-fixture", "passed": True,
                          "manifestSha256": hashlib.sha256(original).hexdigest(),
                          "liveSystemd": False, "guestBoot": False,
                          "stackExecution": False, "terraformExecution": False}))


def observe_without_rpc(original, generation):
    """Retain the original proof without attempting a read while the fixture holds the lock."""
    result = json.loads(json.dumps(original))
    result["ownership"]["generation"] = generation
    return result


if __name__ == "__main__":
    main()
