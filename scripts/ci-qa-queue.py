#!/usr/bin/env python3
"""Real Queue MicroVM acceptance on a fresh, disposable localhost Linux installation."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import urllib.parse
from datetime import datetime, timezone
import uuid

ROOT = Path(__file__).resolve().parent.parent
ORIGIN = "http://127.0.0.1:5523"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


class EngineFailed(Exception):
    pass


OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())


def local_origin(value):
    address = urllib.parse.urlsplit(value)
    if (address.scheme != "http" or address.hostname not in {"127.0.0.1", "localhost"}
            or address.username is not None or address.password is not None
            or address.path not in {"", "/"} or address.query or address.fragment):
        raise ValueError("Queue QA accepts only a localhost HTTP API origin")
    if address.port is None or not 1 <= address.port <= 65535:
        raise ValueError("Queue QA requires an explicit valid API port")
    return f"http://{address.hostname}:{address.port}"


def disk_identity(database, vm_id):
    # Read only the two identity columns. The database also contains credentials.
    script = """import json,pathlib,sqlite3,sys,uuid
path=pathlib.Path(sys.argv[1]).resolve()
with sqlite3.connect(path.as_uri()+'?mode=ro',uri=True) as db:
    row=db.execute("SELECT storage_root,disk_generation FROM vms WHERE id=? AND purpose='queue'",(sys.argv[2],)).fetchone()
if not row or row[0]!='default' or not row[1]:
    raise SystemExit(1)
generation=uuid.UUID(row[1])
disk=path.parent/'vms'/uuid.UUID(sys.argv[2]).hex/'d'/(generation.hex+'.ext4')
if not disk.is_file():
    raise SystemExit(1)
print(json.dumps({'generation':str(generation),'bytes':disk.stat().st_size}))
"""
    result = subprocess.run(["sudo", "-n", "python3", "-c", script, str(database), vm_id],
                            capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise RuntimeError("Could not verify the Queue disk identity")
    return json.loads(result.stdout)


def checkpoint(checks, message):
    checks.append(message)
    print(f"PASS: {message}", flush=True)


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
    if engine and engine["phase"] == "failed" and phase == "running":
        raise EngineFailed(engine.get("error") or "Queue engine failed")
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


def run(args, report):
    checks = report["checks"]
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
        report["vmId"] = running["vmId"]
        report["image"] = running["image"]
        report["diskBefore"] = disk_identity(args.database, engine["vmId"])
        checkpoint(checks, "digest-pinned OCI import and real KVM boot")
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
        checkpoint(checks, "deletion refuses confirmed pending work")
        if args.restart_api:
            subprocess.run(["sudo", "-n", "systemctl", "restart", args.restart_api_unit], check=True, timeout=60)
            recovered = wait("API re-adoption of the broker", lambda: engine_phase("running"))
            if recovered["vmId"] != engine["vmId"]:
                raise RuntimeError("API restart replaced the engine")
            if disk_identity(args.database, engine["vmId"]) != report["diskBefore"]:
                raise RuntimeError("API restart changed the Queue disk identity")
            checkpoint(checks, "API restart re-adopts the same live engine and disk")
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
        report["diskAfter"] = disk_identity(args.database, engine["vmId"])
        if report["diskAfter"] != report["diskBefore"]:
            raise RuntimeError("Engine restart replaced the Queue disk generation")
        checkpoint(checks, "confirmed message, credentials and VM disk survive engine restart")
        client("test-disconnect.mjs", FIRECRAB_TEST_AMQP_PUBLISH_URL=publish,
               FIRECRAB_TEST_AMQP_CONSUME_URL=consume, FIRECRAB_TEST_QUEUE=queue["name"])
        wait("failed delivery retention", lambda: api(path + "/metrics")["failed"] == 1)
        api(path, "DELETE", status=409)
        for message in ["A disconnect preserves confirmed work", "B disconnect redelivers unacknowledged work",
                        "sender and receiver permissions are separate", "delivery limit retains failed work"]:
            checkpoint(checks, message)
        client("test-runtime.mjs", "read", FIRECRAB_QA_AMQP_URL=consume,
               FIRECRAB_QA_QUEUE=queue["name"] + ".failed", FIRECRAB_QA_MESSAGE_ID="",
               FIRECRAB_QA_EXPECT_BODY="repeated-failure")
        wait("empty queues", lambda: all(api(path + "/metrics")[key] == 0
             for key in ["ready", "unacknowledged", "consumers", "failed"]))
        api(path, "DELETE", status=204)
        checkpoint(checks, "empty unused queue deletion")
    finally:
        # The stateful engine has no delete API. The CI job purges its disposable host.
        api("/api/queue/engine/stop", "POST", status=202)
        wait("final engine stop", lambda: engine_phase("stopped"))


def main():
    global ORIGIN
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disposable-host", action="store_true", required=True,
                        help="acknowledge creation of a persistent engine on this fresh local host")
    parser.add_argument("--api", default=ORIGIN, type=local_origin)
    parser.add_argument("--restart-api", action="store_true")
    parser.add_argument("--restart-api-unit", default="firecrab-api")
    parser.add_argument("--database", type=Path, default=Path("/var/lib/firecrab/data/firecrab.db"))
    parser.add_argument("--output", type=Path, default=Path("target/qa/queue.json"))
    args = parser.parse_args()
    if not re.fullmatch(r"firecrab-(api|queue-qa-[0-9a-f-]+)(\.service)?", args.restart_api_unit):
        parser.error("API restart unit must identify Firecrab or its dedicated Queue QA service")
    ORIGIN = args.api
    if platform.system() != "Linux" or not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise RuntimeError("Queue runtime QA requires Linux with accessible /dev/kvm")
    revision = os.environ.get("FIRECRAB_QA_SOURCE_REVISION") or subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT,
        capture_output=True, text=True, check=False).stdout.strip()
    report = {"passed": False, "checks": [], "sourceRevision": revision,
              "architecture": platform.machine(), "hostKernel": platform.release(),
              "startedAt": datetime.now(timezone.utc).isoformat()}
    try:
        run(args, report)
        report["passed"] = True
    finally:
        report["finishedAt"] = datetime.now(timezone.utc).isoformat()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"PASS: {len(report['checks'])} Queue MicroVM acceptance checks")


if __name__ == "__main__":
    try:
        main()
    except (EngineFailed, RuntimeError, subprocess.SubprocessError) as error:
        # No raw subprocess output or connection URLs are included in reports.
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
