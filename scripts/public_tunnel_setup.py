"""Own two disposable MicroService fixtures for public Tunnel acceptance."""

import uuid
import sys


def run_fixture_services(api, wait_for, network, base_spec, fixtures, credentials, evidence, run_public):
    services = []
    errors = []
    evidence["publicFixtureCleanupErrors"] = errors

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def deployment(service):
        return api.call("GET", path(service, "/deployment"))

    def terminal(service):
        def check():
            record = deployment(service)
            if record is None:
                return {"phase": "not_deployed"}
            return record if record["phase"] in ("running", "failed", "degraded", "stopped") else None
        return wait_for(check, timeout=180)

    def create(fixture):
        name = "tunnel-qa-" + uuid.uuid4().hex[:12]
        spec = {**base_spec, "name": name, "domain": name + ".example.test",
                "image": "ghcr.io/team/deploy-qa@" + fixture[0], "port": 8080,
                "healthPath": "/health", "replicas": 1, "env": {}, "secrets": {}}
        if any(item.get("name") == name for item in api.call("GET", "/api/micro-services")):
            raise RuntimeError("refusing an existing fixture service name")
        try:
            identity = api.call("POST", "/api/micro-services", spec)["id"]
        except BaseException:
            # A lost response can leave a saved service even though POST failed.
            try:
                matches = [item for item in api.call("GET", "/api/micro-services") if item.get("name") == name]
                if len(matches) == 1 and all(matches[0].get(key) == spec[key] for key in ("name", "domain", "image", "port")):
                    services.append({"id": matches[0]["id"], "spec": spec})
                elif matches:
                    errors.append("Fixture service ownership is ambiguous: " + name)
            except Exception:
                errors.append("Could not verify fixture service creation: " + name)
            raise
        service = {"id": identity, "spec": spec}
        services.append(service)
        api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
        record = terminal(service)
        if record["phase"] != "running":
            raise RuntimeError("public Tunnel fixture did not start")
        return service

    try:
        a, b = [create(fixture) for fixture in fixtures]
        run_public(api, wait_for, a, b, credentials, evidence)
    finally:
        original_error = sys.exc_info()[0] is not None
        for service in reversed(services):
            try:
                record = terminal(service)
                if record["phase"] not in ("stopped", "not_deployed"):
                    api.call("POST", path(service, "/stop"))
                    wait_for(lambda: deployment(service)["phase"] == "stopped", timeout=180)
                api.call("DELETE", path(service))
            except Exception:
                errors.append("Could not remove owned fixture service " + service["spec"]["name"])
        if errors and not original_error:
            raise RuntimeError("Public Tunnel fixture cleanup failed; see report")
