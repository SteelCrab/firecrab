"""Real CLI/Action -> private verified TLS -> Nginx -> scoped Deploy acceptance."""
import contextlib
import http.client
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import threading
import time
import uuid
from deploy_auto_qa import trusted_registry


def run_ci(api, wait_for, traffic, network, spec_a, fixtures, port, evidence):
    metrics = {"requests": 0, "httpFailures": [], "monitorErrors": [], "maxLiveVMs": 0,
               "maxLiveVCPUs": 0, "httpTimeoutSeconds": 5, "stages": []}
    evidence["ciDeploy"] = metrics
    services = []
    stop = threading.Event()
    started = time.monotonic()
    loader = None
    cli = shutil.which("firecrab")
    assert cli, "installed current CLI is required"
    action = Path(__file__).resolve().parents[1] / ".github/actions/deploy/deploy.py"

    def path(service, suffix=""):
        return "/api/micro-services/" + service["id"] + suffix

    def current(service):
        return api.call("GET", path(service, "/deployment"))

    def ready(service, digest):
        record = current(service)
        return record if record["phase"] == "running" and record["digest"] == digest else None

    def create(domain):
        spec = {**spec_a, "name": "ci-" + uuid.uuid4().hex[:8], "domain": domain, "replicas": 1, "startupTimeout": 3}
        service = {"id": api.call("POST", "/api/micro-services", spec)["id"], "spec": spec}
        services.append(service)
        api.call("POST", path(service, "/deployment"), {"microNetworkId": network})
        return service, wait_for(lambda: ready(service, fixtures[0][0]))

    def monitor():
        while not stop.is_set():
            for service in services.copy():
                try:
                    response = traffic("127.0.0.1", port, service["spec"]["domain"])
                    metrics["requests"] += 1
                    if response not in [(200, "release-A"), (200, "release-B"), (200, "release-C")]:
                        metrics["httpFailures"].append(response)
                except Exception as error:
                    metrics["httpFailures"].append(str(error))
            try:
                live = [vm for vm in api.call("GET", "/api/vms") if vm["state"] not in ["created", "stopped", "failed"]]
                metrics["maxLiveVMs"] = max(metrics["maxLiveVMs"], len(live))
                metrics["maxLiveVCPUs"] = max(metrics["maxLiveVCPUs"], sum(vm["cpu"] for vm in live))
            except (OSError, http.client.HTTPException):
                pass
            except Exception as error:
                metrics["monitorErrors"].append(str(error))
            stop.wait(.05)

    with trusted_registry(fixtures, proxy_hosts=("ci.example.test",)) as registry:
        try:
            api.ready()
            registry.proxies["ci.example.test"] = port
            service, original = create("ci.example.test")
            unrelated, other = create("ci-unrelated.example.test")
            issued = api.call("POST", path(service, "/deploy-tokens"), {"name": "Acceptance"})
            short = api.call("POST", path(service, "/deploy-tokens"), {"name": "Expiry", "expiresInSeconds": 60})
            token = issued["token"]
            loader = threading.Thread(target=monitor, daemon=True)
            loader.start()
            url = "https://ci.example.test/__firecrab/deploy"
            environment = {**os.environ, "SSL_CERT_FILE": str(registry.ca_file),
                           "NO_PROXY": os.environ.get("NO_PROXY", "") + ",ci.example.test,127.0.0.1,localhost",
                           "FIRECRAB_DEPLOY_TOKEN": token, "CI_DEPLOY_URL": url, "CI_DEPLOY_TOKEN": token,
                           "CI_DEPLOY_TIMEOUT": "180", "CI_DEPLOY_RUN_KEY": "", "CI_DEPLOY_STRATEGY": "rolling"}
            environment.pop("GITHUB_OUTPUT", None)
            environment["no_proxy"] = environment["NO_PROXY"]
            context = ssl.create_default_context(cafile=str(registry.ca_file))

            def https(method="GET", suffix="", body=None, credential=token):
                with contextlib.closing(http.client.HTTPSConnection("ci.example.test", context=context, timeout=15)) as connection:
                    connection.request(method, "/__firecrab/deploy" + suffix,
                                       json.dumps(body) if body is not None else None,
                                       {"Authorization": "Bearer " + credential, "Content-Type": "application/json"})
                    response = connection.getresponse()
                    raw = response.read()
                    assert token.encode() not in raw
                    return response.status, json.loads(raw) if raw else None

            def invoke(kind, image, operation, expected=None, wanted=0, env=None, timeout=180):
                image = "ghcr.io/team/deploy-qa@" + image
                child = {**environment, **(env or {}), "CI_DEPLOY_IMAGE": image, "CI_DEPLOY_OPERATION": operation,
                         "CI_DEPLOY_EXPECTED": expected or ""}
                command = [sys.executable, str(action)] if kind == "action" else [cli, "microservice", "ci-deploy", "--url", url, "--image", image,
                    "--operation-id", operation, "--timeout", str(timeout)] + (["--expected-release", expected] if expected else [])
                result = subprocess.run(command, env=child, text=True, capture_output=True, timeout=timeout + 20)
                assert token not in result.stdout + result.stderr, "client exposed a Deploy token"
                assert result.returncode == wanted, {"client": kind, "exit": result.returncode, "expected": wanted, "output": result.stdout + result.stderr}
                return result

            listed = api.call("GET", path(service, "/deploy-tokens"))
            assert token not in json.dumps(listed) and len(listed) == 2
            assert https()[0] == 200
            assert https("POST", body={"operationId": str(uuid.uuid4()), "expectedReleaseId": original["id"],
                "image": "ghcr.io/team/deploy-qa@" + fixtures[1][0], "env": {"INVALID": "forbidden"}})[0] == 400
            assert https(suffix="/api/vms")[0] == 404
            with contextlib.closing(http.client.HTTPConnection("127.0.0.1", 5525, timeout=10)) as foreign:
                foreign.request("GET", "/api/ci/services/" + unrelated["id"] + "/deployments", headers={"Authorization": "Bearer " + token})
                response = foreign.getresponse()
                assert response.status == 403
                response.read()
            assert https("POST", body={"operationId": str(uuid.uuid4()), "expectedReleaseId": original["id"],
                "image": "ghcr.io/team/other@" + fixtures[1][0]})[0] == 403
            before_tls = len(registry.proxy_calls)
            rejected = invoke("cli", fixtures[1][0], str(uuid.uuid4()), wanted=1,
                env={"SSL_CERT_FILE": "/etc/ssl/certs/ca-certificates.crt"}, timeout=2)
            assert "timed out" in rejected.stderr
            assert len(registry.proxy_calls) == before_tls, "untrusted TLS must not receive credentials"
            metrics["stages"].append("verified TLS and fixed Nginx mapping expose only token-authenticated image/status paths")

            operation = str(uuid.uuid4())
            first = subprocess.Popen([cli, "microservice", "ci-deploy", "--url", url, "--image", "ghcr.io/team/deploy-qa@" + fixtures[1][0],
                "--operation-id", operation, "--expected-release", original["id"], "--timeout", "180"], env=environment,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                wait_for(lambda: current(service)["id"] == operation)
                metrics["phaseAtRestart"] = current(service)["phase"]
                first.terminate()
                stdout, stderr = first.communicate(timeout=10)
                assert token not in stdout + stderr
            finally:
                if first.poll() is None:
                    first.kill()
                    first.communicate(timeout=10)
            subprocess.run(["systemctl", "restart", "firecrab-api.service"], check=True)
            api.ready()
            invoke("action", fixtures[1][0], operation, original["id"])
            active = wait_for(lambda: ready(service, fixtures[1][0]))
            assert active["id"] == operation
            metrics["stages"].append("CLI acceptance survives client disconnect and API restart; Action resumes the same pinned operation")

            failed = str(uuid.uuid4())
            invoke("action", fixtures[2][0], failed, active["id"], wanted=1)
            restored = wait_for(lambda: ready(service, fixtures[1][0]))
            assert restored["id"] == active["id"]
            status, receipt = https(suffix="/" + failed)
            assert status == 200 and receipt["completed"] and not receipt["succeeded"]
            attempt = api.call("GET", path(service, "/deployments/" + failed))
            assert attempt["phase"] == "failed" and "health" in (attempt["update"]["failure"] or "").lower(), "CI failure did not reach the unhealthy guest check"
            invoke("cli", fixtures[2][0], failed, active["id"], wanted=1)
            metrics["stages"].append("unhealthy CI Deploy fails both clients while the previous release remains available")

            completed = str(uuid.uuid4())
            invoke("action", fixtures[3][0], completed, active["id"])
            live = wait_for(lambda: ready(service, fixtures[3][0]))
            invoke("cli", fixtures[3][0], completed, active["id"])
            assert live["id"] == completed and current(unrelated)["id"] == other["id"]
            deadline = short["credential"]["expiresAt"] + 1
            while time.time() < deadline:
                time.sleep(min(1, deadline - time.time()))
            assert https(credential=short["token"])[0] == 401
            api.call("DELETE", path(service, "/deploy-tokens/" + issued["credential"]["id"]))
            assert https()[0] == 401
            invoke("cli", fixtures[0][0], str(uuid.uuid4()), wanted=1)
            invoke("action", fixtures[0][0], str(uuid.uuid4()), wanted=1)
            assert current(service)["id"] == completed and current(unrelated)["id"] == other["id"]
            metrics["stages"].append("expiry and revocation reject CI without new work; unrelated service is unchanged")
        finally:
            stop.set()
            if loader:
                loader.join(timeout=10)
            for service in services:
                api.ready()
                api.call("POST", path(service, "/stop"))
                wait_for(lambda: current(service)["phase"] == "stopped")
                api.call("DELETE", path(service))
            metrics["durationSeconds"] = round(time.monotonic() - started, 3)
    api.ready()
    assert not api.call("GET", "/api/vms")
    assert metrics["requests"] > 20 and not metrics["httpFailures"] and not metrics["monitorErrors"], metrics
    assert metrics["maxLiveVMs"] <= 4 and metrics["maxLiveVCPUs"] <= 4, metrics
    metrics["cleanup"] = "VMs, services, tokens, TLS edge, private trust and hosts mappings removed"
    print(f"PASS scoped CLI/Action CI Deploy and restart/failure/expiry/revocation; {metrics['requests']} HTTP requests, zero errors", flush=True)
