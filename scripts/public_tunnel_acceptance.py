"""Public Tunnel acceptance for two fixture services on a disposable host.

The caller owns fixture VM, Nginx, and dashboard reservation cleanup. This
module manages only the two connections whose tokens it can verify locally.
"""

import contextlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import threading
import time

from public_tunnel_credentials import verify_registration, verify_revoked


_probe_spec = importlib.util.spec_from_file_location(
    "public_tunnel_probe", Path(__file__).with_name("check-public-tunnel-app.py")
)
probe = importlib.util.module_from_spec(_probe_spec)
_probe_spec.loader.exec_module(probe)

STORE = Path("/etc/firecrab/pipe/firecrab-addresses.json")
ENDPOINT = "https://tunnel.firecrab.dev"


class AcceptanceError(Exception):
    """The acceptance failed without exposing a token or response body."""


def owned_binding(store, credential):
    """Refuse to delete an address unless the committed ownership matches."""
    if not store.exists():
        return None
    try:
        value = json.loads(store.read_text())
        if value.get("endpoint") != ENDPOINT or value.get("protocol") != "register_v1":
            raise AcceptanceError("unexpected address store endpoint or protocol")
        matches = [entry for entry in value["bindings"] if entry["subdomain"] == credential.label]
        if len(matches) > 1:
            raise AcceptanceError("duplicate address ownership")
        if not matches:
            return None
        entry = matches[0]
        if entry.get("token") != credential.token or entry.get("ownedConnector") is not True:
            raise AcceptanceError("address ownership does not match the test token")
        return entry["connectorId"]
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise AcceptanceError("cannot safely inspect address ownership") from error


def run_cli(cli, *args, credential=None):
    """Pass a token through an inherited anonymous file, never arguments or logs."""
    env = {"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"), "HOME": "/root"}
    descriptor = None
    try:
        if credential is not None:
            descriptor = os.memfd_create("firecrab-qa-token", flags=0)
            os.fchmod(descriptor, 0o600)
            os.write(descriptor, credential.token.encode("ascii"))
            os.lseek(descriptor, 0, os.SEEK_SET)
            args = (*args, "--token-file", f"/proc/self/fd/{descriptor}")
        output = subprocess.run([cli, "pipe", *args], env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=120,
                                pass_fds=(descriptor,) if descriptor is not None else ())
        if output.returncode:
            raise AcceptanceError("Firecrab address command failed")
    except (OSError, subprocess.SubprocessError) as error:
        raise AcceptanceError("could not run Firecrab address command") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)


def expect_fixture(ip, port, expected):
    try:
        with contextlib.closing(http.client.HTTPConnection(ip, port, timeout=5)) as connection:
            connection.request("GET", "/app")
            response = connection.getresponse()
            if response.status != 200 or response.read(100) != expected:
                raise AcceptanceError("local VM fixture did not match")
    except (OSError, http.client.HTTPException) as error:
        raise AcceptanceError("local VM fixture is unavailable") from error


def fixture_targets(api, services):
    """Require exactly the two declared test services and their active VMs."""
    if os.geteuid() != 0 or not Path("/dev/kvm").exists():
        raise AcceptanceError("root and a disposable Linux KVM host are required")
    expected_ids = {service["id"] for service in services}
    if len(expected_ids) != 2:
        raise AcceptanceError("two different fixture services are required")
    listed = api.call("GET", "/api/micro-services")
    if {item["id"] for item in listed} != expected_ids:
        raise AcceptanceError("refusing a host with other services")
    listed_by_id = {item["id"]: item for item in listed}
    deployments, vm_ids, route_owners = [], set(), {}
    for service, label in zip(services, ("A", "B")):
        spec = service["spec"]
        saved = listed_by_id[service["id"]]
        if any(saved.get(key) != spec[key] for key in ("name", "domain", "port")):
            raise AcceptanceError("fixture identity does not match saved service")
        if not spec["name"].startswith("tunnel-qa-") or not spec["domain"].endswith(".example.test"):
            raise AcceptanceError("refusing a non-test service or domain")
        deployment = api.call("GET", "/api/micro-services/" + service["id"] + "/deployment")
        instances = deployment["instances"]
        if deployment["phase"] != "running" or len(instances) != 1 or not instances[0].get("ipv4"):
            raise AcceptanceError("each fixture needs one running VM")
        if not deployment.get("id"):
            raise AcceptanceError("fixture deployment identity is missing")
        owner = {"serviceId": service["id"], "deploymentId": deployment["id"]}
        route_owners[spec["domain"]] = owner
        route_owners[spec["name"] + ".internal"] = owner
        instance = instances[0]
        vm_ids.add(instance["vmId"])
        expect_fixture(instance["ipv4"], spec["port"], ("fixture-" + label).encode())
        deployments.append(instance["ipv4"] + ":" + str(spec["port"]))
    if len(vm_ids) != 2 or {vm["id"] for vm in api.call("GET", "/api/vms")} != vm_ids:
        raise AcceptanceError("refusing a host with other VMs")
    snapshot = api.call("GET", "/api/pipe")
    config = snapshot.get("config") or {}
    if snapshot.get("firecrabTunnels") or config.get("quickTunnels") or config.get("quickTunnel") or config.get("tunnel"):
        raise AcceptanceError("refusing existing tunnel connections")
    routes = config.get("routes", [])
    if (len(routes) != len(route_owners) or {route.get("host") for route in routes} != set(route_owners)
            or any(route.get("owner") != route_owners.get(route.get("host"))
                   or route.get("pathPrefix", "/") != "/" for route in routes)):
        raise AcceptanceError("refusing unexpected or unowned Nginx routes")
    if STORE.exists():
        try:
            store = json.loads(STORE.read_text())
            if store.get("endpoint") != ENDPOINT or store.get("bindings"):
                raise AcceptanceError("refusing existing or non-production address store")
        except (OSError, ValueError) as error:
            raise AcceptanceError("cannot safely inspect the address store") from error
    return deployments


def public_ok(credential, expected):
    host = probe.validate_host(credential.label)
    context = ssl.create_default_context()
    probe.check_https(host, "/app", expected, context)
    probe.check_websocket(host, "/ws", context)
    return True


def public_offline(credential):
    host = probe.validate_host(credential.label)
    try:
        with contextlib.closing(http.client.HTTPSConnection(host, timeout=15,
                                                          context=ssl.create_default_context())) as connection:
            connection.request("GET", "/app", headers={"Cache-Control": "no-cache"})
            response = connection.getresponse()
            response.read(4096)
            return response.status in (404, 410, 502, 503, 504)
    except (OSError, http.client.HTTPException):
        return False


def ready(wait_for, credential, expected):
    def check():
        try:
            return public_ok(credential, expected)
        except probe.CheckError:
            return False
    return wait_for(limited(check), timeout=180, message="public fixture did not become ready")


def wait_offline(wait_for, credential):
    return wait_for(limited(lambda: public_offline(credential)), timeout=180,
                    message="public address did not become offline")


def limited(check):
    """Respect production request quotas while the caller polls readiness."""
    next_request = 0.0

    def poll():
        nonlocal next_request
        now = time.monotonic()
        if now < next_request:
            return False
        next_request = now + 5
        return check()

    return poll


def restart_api(api, wait_for, credentials, expected):
    stop = threading.Event()
    results = []

    def monitor():
        while not stop.is_set():
            try:
                for credential, body in zip(credentials, expected):
                    probe.check_https(probe.validate_host(credential.label), "/app", body,
                                      ssl.create_default_context())
                results.append(True)
            except probe.CheckError:
                results.append(False)
            # Public accounts have request quotas. Keep monitoring bounded.
            stop.wait(2)

    worker = threading.Thread(target=monitor, daemon=True)
    worker.start()
    try:
        wait_for(lambda: len(results) >= 1, timeout=30, message="restart traffic monitor did not start")
        if not all(results):
            raise AcceptanceError("public traffic failed before API restart")
        subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
        api.ready()
        observed = len(results)
        wait_for(lambda: len(results) > observed, timeout=30, message="restart traffic monitor did not continue")
    finally:
        stop.set()
        worker.join(timeout=35)
    if worker.is_alive() or not results or not all(results):
        raise AcceptanceError("public traffic failed during API restart")
    return len(results)


def run_public(api, wait_for, service_a, service_b, credentials, evidence):
    """Run acceptance after the caller creates two exclusive fixture services.

    The operator must revoke both test tokens at the prompts. Empty input only
    resumes the test: an actual rejected registration and offline response are
    required. Dashboard reservations and VM cleanup remain the caller's duty.
    """
    metrics = {"status": "failed", "stages": [], "cleanupErrors": [],
               "dashboardReservations": "operator_cleanup_required",
               "dashboardRevocations": "operator_verification_required",
               "connectionChecks": "not_completed",
               "tokenAuthentication": {credential.label: "not_verified" for credential in credentials}}
    evidence["publicTunnel"] = metrics
    if len(credentials) != 2 or credentials[0].label == credentials[1].label:
        raise AcceptanceError("two distinct test credentials are required")
    cli = shutil.which("firecrab")
    if not cli:
        raise AcceptanceError("installed Firecrab CLI is required")
    targets = fixture_targets(api, [service_a, service_b])
    for credential in credentials:
        verify_registration(credential)
        metrics["tokenAuthentication"][credential.label] = "accepted"
    metrics["stages"].append("authenticated registration for both test addresses")
    attempted = []
    primary, peer = credentials
    try:
        for credential, target, body in zip(credentials, targets, (b"fixture-A", b"fixture-B")):
            attempted.append(credential)
            run_cli(cli, "address", "connect", "--app", target, credential=credential)
            ready(wait_for, credential, body)
        metrics["stages"].append("two independent VM app connections with HTTPS and WebSocket echo")

        run_cli(cli, "address", "connect", primary.label, "--nginx", service_b["spec"]["domain"])
        ready(wait_for, primary, b"fixture-B")
        ready(wait_for, peer, b"fixture-B")
        run_cli(cli, "address", "connect", primary.label, "--app", targets[0])
        ready(wait_for, primary, b"fixture-A")
        metrics["stages"].append("retarget to saved Nginx route and back without disturbing peer")

        metrics["restartSamples"] = restart_api(api, wait_for, credentials, (b"fixture-A", b"fixture-B"))
        for credential, body in zip(credentials, (b"fixture-A", b"fixture-B")):
            ready(wait_for, credential, body)
        metrics["stages"].append("HTTPS continuity across API restart and WebSocket echo afterward")

        if owned_binding(STORE, primary) is None:
            raise AcceptanceError("primary local ownership disappeared")
        run_cli(cli, "address", "delete", primary.label)
        wait_offline(wait_for, primary)
        ready(wait_for, peer, b"fixture-B")
        run_cli(cli, "address", "connect", "--app", targets[0], credential=primary)
        ready(wait_for, primary, b"fixture-A")
        metrics["stages"].append("disconnect and reconnect preserve the peer address")

        for credential, other in ((primary, peer), (peer, None)):
            print(f"Revoke test token for {credential.label} in the dashboard, then press Enter.", flush=True)
            input()
            verify_revoked(credential)
            metrics["tokenAuthentication"][credential.label] = "registration_rejected"
            wait_offline(wait_for, credential)
            if other:
                ready(wait_for, other, b"fixture-B")
            metrics["stages"].append("registration rejected and public address offline after operator action: " + credential.label)
        metrics["connectionChecks"] = "passed"
        metrics["status"] = "pending_operator_verification"
    finally:
        original_error = sys.exc_info()[0] is not None
        for credential in reversed(attempted):
            try:
                if owned_binding(STORE, credential) is not None:
                    run_cli(cli, "address", "delete", credential.label)
                if owned_binding(STORE, credential) is not None:
                    raise AcceptanceError("local address remains")
            except Exception:
                metrics["cleanupErrors"].append("local connection cleanup failed: " + credential.label)
        try:
            config = (api.call("GET", "/api/pipe").get("config") or {})
            if config.get("quickTunnels") or config.get("quickTunnel"):
                metrics["cleanupErrors"].append("connector cleanup is incomplete or unverified")
        except Exception:
            metrics["cleanupErrors"].append("connector cleanup is unverified")
        if metrics["cleanupErrors"]:
            metrics["status"] = "failed"
            if not original_error:
                raise AcceptanceError("public test connection cleanup failed")
