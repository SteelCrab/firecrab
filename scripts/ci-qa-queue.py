#!/usr/bin/env python3
"""Real Queue MicroVM acceptance on a fresh, disposable localhost Linux installation."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
ORIGIN = "http://127.0.0.1:5523"
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def api(path, method="GET", body=None, status=200):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(ORIGIN + path, data=data, method=method,
                                    headers={"Content-Type": "application/json", "Origin": ORIGIN})
    try:
        response = OPENER.open(request, timeout=20)
    except urllib.error.HTTPError as error:
        if error.code == status:
            error.close()
            return None
        raise RuntimeError(f"{method} {path}: HTTP {error.code}") from None
    except OSError:
        raise RuntimeError("Local Firecrab API is unavailable") from None
    with response:
        if response.status != status:
            raise RuntimeError(f"{method} {path}: unexpected status")
        content = response.read(1024 * 1024)
        return json.loads(content) if content else None


def wait(description, probe, seconds=240):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (RuntimeError, ValueError):
            pass
        time.sleep(1)
    raise RuntimeError(f"Timed out waiting for {description}")


def engine_phase(phase):
    engine = api("/api/queue/engine")
    return engine if engine and engine["phase"] == phase else None


def client(script, mode=None, **environment):
    command = ["node", str(ROOT / "examples/queue" / script)]
    if mode:
        command.append(mode)
    result = subprocess.run(command, env=dict(os.environ, **environment),
                            capture_output=True, text=True, timeout=75)
    if result.returncode:
        # Library output can include AMQP credentials. Do not relay stderr or stdout.
        raise RuntimeError("RabbitMQ client assertion failed")


def run(restart_api, checks):
    if api("/api/queue/engine") is not None:
        raise RuntimeError("Queue runtime QA requires an installation with no existing engine")
    network = api("/api/micro-networks", "POST", {
        "name": "qa-queue-" + uuid.uuid4().hex[:8],
        "subnetCidr": "172.28.250.0/24", "internetEnabled": True,
    }, status=201)
    engine = api("/api/queue/engine", "POST", {
        "microNetworkId": network["id"], "cpu": 2, "ram": 1024, "diskGb": 4,
    }, status=202)
    try:
        running = wait("RabbitMQ OCI import and MicroVM boot", lambda: engine_phase("running"), 1200)
        if running["vmId"] != engine["vmId"]:
            raise RuntimeError("Engine identity changed during installation")
        checks.append("digest-pinned OCI import and real KVM boot")
        queue = api("/api/queues", "POST", {
            "name": "qa-" + uuid.uuid4().hex[:8], "deliveryLimit": 2, "maxBytes": 64 * 1024 * 1024,
        }, status=202)
        path = "/api/queues/" + queue["id"]
        wait("Queue provisioning", lambda: next((q for q in api("/api/queues")
             if q["id"] == queue["id"] and q["phase"] == "ready"), None))
        publish = api(path + "/connection/publish", "POST")["url"]
        consume = api(path + "/connection/consume", "POST")["url"]
        message_id = uuid.uuid4().hex
        client("test-runtime.mjs", "seed", FIRECRAB_QA_AMQP_URL=publish,
               FIRECRAB_QA_QUEUE=queue["name"], FIRECRAB_QA_MESSAGE_ID=message_id)
        wait("confirmed work", lambda: api(path + "/metrics")["ready"] == 1)
        api(path, "DELETE", status=409)
        checks.append("deletion refuses confirmed pending work")
        if restart_api:
            subprocess.run(["sudo", "systemctl", "restart", "firecrab-api"], check=True, timeout=60)
            recovered = wait("API re-adoption of the broker", lambda: engine_phase("running"))
            if recovered["vmId"] != engine["vmId"]:
                raise RuntimeError("API restart replaced the engine")
            checks.append("API restart re-adopts the same live engine")
        api("/api/queue/engine/stop", "POST", status=202)
        wait("engine stop", lambda: engine_phase("stopped"))
        api("/api/queue/engine/start", "POST", status=202)
        restarted = wait("persistent engine restart", lambda: engine_phase("running"))
        if restarted["vmId"] != engine["vmId"]:
            raise RuntimeError("Engine restart replaced the VM")
        if api(path + "/connection/publish", "POST")["url"] != publish:
            raise RuntimeError("Publisher connection changed after restart")
        if api(path + "/connection/consume", "POST")["url"] != consume:
            raise RuntimeError("Consumer connection changed after restart")
        client("test-runtime.mjs", "read", FIRECRAB_QA_AMQP_URL=consume,
               FIRECRAB_QA_QUEUE=queue["name"], FIRECRAB_QA_MESSAGE_ID=message_id,
               FIRECRAB_QA_EXPECT_BODY="persistent-runtime-probe")
        wait("acknowledged work removal", lambda: api(path + "/metrics")["ready"] == 0)
        checks.append("confirmed message, credentials and VM disk survive engine restart")
        client("test-disconnect.mjs", FIRECRAB_TEST_AMQP_PUBLISH_URL=publish,
               FIRECRAB_TEST_AMQP_CONSUME_URL=consume, FIRECRAB_TEST_QUEUE=queue["name"])
        wait("failed delivery retention", lambda: api(path + "/metrics")["failed"] == 1)
        api(path, "DELETE", status=409)
        checks.extend(["A disconnect preserves confirmed work", "B disconnect redelivers unacknowledged work",
                       "sender and receiver permissions are separate", "delivery limit retains failed work"])
        client("test-runtime.mjs", "read", FIRECRAB_QA_AMQP_URL=consume,
               FIRECRAB_QA_QUEUE=queue["name"] + ".failed", FIRECRAB_QA_MESSAGE_ID="",
               FIRECRAB_QA_EXPECT_BODY="repeated-failure")
        wait("empty queues", lambda: all(api(path + "/metrics")[key] == 0
             for key in ["ready", "unacknowledged", "consumers", "failed"]))
        api(path, "DELETE", status=204)
        checks.append("empty unused queue deletion")
    finally:
        # The stateful engine has no delete API. The CI job purges its disposable host.
        api("/api/queue/engine/stop", "POST", status=202)
        wait("final engine stop", lambda: engine_phase("stopped"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disposable-host", action="store_true", required=True,
                        help="acknowledge creation of a persistent engine on this fresh local host")
    parser.add_argument("--restart-api", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("target/qa/queue.json"))
    args = parser.parse_args()
    if platform.system() != "Linux" or not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise RuntimeError("Queue runtime QA requires Linux with accessible /dev/kvm")
    checks = []
    report = {"passed": False, "checks": checks}
    try:
        run(args.restart_api, checks)
        report["passed"] = True
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"PASS: {len(checks)} Queue MicroVM acceptance checks")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError) as error:
        # No raw subprocess output or connection URLs are included in reports.
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
