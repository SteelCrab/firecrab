#!/usr/bin/env python3
"""Opt-in real-model/MicroVM gate on a disposable Linux API host.

Credentials stay in Pipe AI. Only a connection and VM named for this run may be
used. The connection is disconnected; the borrowed VM/network are preserved.
Fixtures, transport-only tests and a successful assistant message do not pass.
"""

import argparse
import datetime
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener
import uuid


class QaFailure(Exception):
    """A failed gate; never include credentials or a raw HTTP error body."""


def require(condition, message):
    if not condition:
        raise QaFailure(message)


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Api:
    def __init__(self, base):
        parsed = urlsplit(base)
        require(parsed.scheme == "http" and parsed.hostname == "127.0.0.1"
                and parsed.port and not parsed.username and not parsed.password
                and parsed.path in ("", "/") and not parsed.query and not parsed.fragment,
                "Run on the Linux API host with an explicit http://127.0.0.1:PORT URL")
        self.base = base.rstrip("/")
        self.http = build_opener(ProxyHandler({}), NoRedirect())

    def request(self, method, path, body=None, allowed=(200,)):
        request = Request(self.base + path, method=method,
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json"})
        try:
            response = self.http.open(request, timeout=360)
        except HTTPError as error:
            response = error
        with response:
            raw = response.read(2 * 1024 * 1024 + 1)
            require(len(raw) <= 2 * 1024 * 1024, "API response exceeds the QA limit")
            document = json.loads(raw)
            code = document.get("error", {}).get("code", "unknown") if isinstance(document, dict) else "unknown"
            require(response.code in allowed, f"{method} {path}: HTTP {response.code}, code={code}")
            return response.code, document


def guest_command(run_id):
    return f"""set -eu
test "$(id -u)" -ne 0
test "$(awk '/^NoNewPrivs:/ {{print $2}}' /proc/self/status)" = 1
test -z "${{OPENAI_API_KEY-}}${{ANTHROPIC_API_KEY-}}${{SSH_AUTH_SOCK-}}"
nonce=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \\n')
printf '%s\\n' "$nonce" > guest-result.txt
printf 'FIRECRAB414:{run_id}:stdout:%s\\n' "$nonce"
printf 'FIRECRAB414:{run_id}:stderr\\n' >&2
exit 7"""


def validate_result(task, project, vm, selection, run_id):
    require(task.get("projectId") == project and task.get("selection") == selection,
            "Task lost its project or effective model selection")
    require(task.get("state") == "result", f"Task did not finish: {task.get('state')}, {task.get('errorCode')}")
    runtime = task.get("runtime") or {}
    require(runtime.get("vmId") == vm and runtime.get("runtimeId"), "Missing admitted VM/runtime reference")
    require(uuid.UUID(task["attemptId"]).int and uuid.UUID(runtime["runtimeId"]).int,
            "Attempt/runtime identity cannot be nil")
    events = task.get("events", [])
    require(len(events) == 1 and events[0].get("name") == "run_command",
            "Expected exactly one real guest command, without other tool calls")
    event = events[0]
    require(json.loads(event["input"]).get("command") == guest_command(run_id),
            "Model changed the authorized QA command")
    match = re.fullmatch(rf"FIRECRAB414:{re.escape(run_id)}:stdout:([0-9a-f]{{32}})\n", event.get("output", ""))
    require(match is not None, "Missing unpredictable guest-generated stdout nonce")
    nonce = match.group(1)
    require(event.get("stderr") == f"FIRECRAB414:{run_id}:stderr\n" and event.get("exitCode") == 7,
            "Separate stderr or actual nonzero guest exit status was lost")
    require(nonce in task.get("result", ""), "The model did not receive and return the guest result")
    changes = task.get("changes", [])
    require(len(changes) == 1 and changes[0].get("path") == "guest-result.txt"
            and changes[0].get("before") == "not executed\n"
            and changes[0].get("after") == nonce + "\n" and not changes[0].get("applied"),
            "The guest result was not retained as an unapplied source change")
    return nonce


def wait_task(api, key, report, output, timeout=600):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _, task = api.request("GET", f"/api/ai-sandbox/tasks/{key}")
        report["tasks"][key] = task
        save_report(output, report)
        if task["state"] not in ("waiting", "accepted", "in_progress"):
            return task
        time.sleep(1)
    raise QaFailure(f"Task deadline exceeded; inspect recorded task {key} before retrying")


def save_report(output, report):
    temporary = output.with_name(f".{output.name}.{uuid.uuid4()}.tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(report, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def owned_inputs(api, run_id, connection, vm):
    label = f"sandbox-foundation-{run_id}"
    _, connections = api.request("GET", "/api/pipe/ai/connections")
    account = next((c for c in connections if c["id"] == connection), None)
    require(account is not None and account.get("name") == label,
            "Connection must be created for this run; borrowed credentials cannot be disconnected")
    _, machine = api.request("GET", f"/api/vms/{vm}")
    require(machine.get("id") == vm and machine.get("name") == label
            and machine.get("purpose", "instance") == "instance" and machine.get("state") == "running",
            "Select this run's running disposable user MicroVM")
    _, projects = api.request("GET", "/api/ai-sandbox/projects")
    require(not any(p["spec"].get("vmId") == vm or (p["spec"].get("selection") or {}).get("connectionId") == connection
                    for p in projects), "VM or connection is already referenced by another project")
    return account, machine, {c["id"] for c in connections}


def source_git(source, *args):
    command = ["/usr/bin/git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
               "-c", "user.name=Firecrab QA", "-c", "user.email=qa@localhost", "-C", str(source), *args]
    return subprocess.run(command, check=True, capture_output=True, text=True,
                          env={"PATH": "/usr/bin:/bin", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}).stdout.strip()


def run_gate(args, api):
    run_id, connection, vm = map(str, (args.run_id, args.connection, args.vm))
    require(args.default_model != args.override_model, "Use two different account-visible models")
    account, machine, original_connections = owned_inputs(api, run_id, connection, vm)
    results = args.results_dir.resolve()
    results.mkdir(mode=0o700, parents=True, exist_ok=False)
    output = results / "summary.json"
    source = results / "source"
    source.mkdir(mode=0o700)
    (source / "guest-result.txt").write_text("not executed\n")
    source_git(source, "init", "-q")
    source_git(source, "add", "guest-result.txt")
    source_git(source, "commit", "-q", "--no-gpg-sign", "-m", "Sandbox foundation QA baseline")
    project = str(uuid.uuid4())
    report = {"runId": run_id, "status": "RUNNING", "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "platform": platform.platform(), "api": args.api, "projectId": project,
              "sourcePath": str(source), "sourceCommit": source_git(source, "rev-parse", "HEAD"),
              "connection": {k: account[k] for k in ("id", "provider", "authentication")},
              "vm": {k: machine[k] for k in ("id", "name", "state", "microNetworkId", "template", "templateVersion")},
              "checks": {}, "tasks": {}, "retainedOnFailure": True}
    save_report(output, report)
    try:
        _, catalogue = api.request("GET", f"/api/ai-sandbox/connections/{connection}/models?discover=true")
        available = {m["id"] for m in catalogue["models"]}
        require({args.default_model, args.override_model} <= available, "Requested models are not account-visible")
        for model in (args.default_model, args.override_model):
            _, proof = api.request("POST", f"/api/ai-sandbox/connections/{connection}/verify", {"model": model, "tools": True})
            require(proof.get("verified") and proof.get("toolsVerified"), "Real model native-tool verification failed")
        report["checks"]["nativeModelPermissions"] = "PASS"
        default = {"connectionId": connection, "model": args.default_model}
        spec = {"name": f"sandbox-foundation-{run_id}", "sourcePath": str(source), "vmId": vm,
                "selection": default, "instructions": "Use only the exact command authorized by this QA task."}
        api.request("PUT", f"/api/ai-sandbox/projects/{project}", spec)
        nonces = []
        attempts = set()
        runtime = None
        for label, model in (("projectDefault", args.default_model), ("taskOverride", args.override_model)):
            key = str(uuid.uuid4())
            selected = {"connectionId": connection, "model": model}
            prompt = ("Call run_command exactly once with timeout_seconds=30 and this exact command. "
                      "Do not use any other tool or retry its deliberate exit 7. After receiving the result, "
                      "reply with the actual stdout nonce, stderr, and exit status. The nonce must come from the guest.\n```sh\n"
                      + guest_command(run_id) + "\n```")
            request = {"kind": "code", "input": prompt}
            if label == "taskOverride":
                request["selection"] = selected
            report["tasks"][key] = {"id": key, "request": request}
            save_report(output, report)
            _, admitted = api.request("PUT", f"/api/ai-sandbox/projects/{project}/tasks/{key}", request, (200, 202))
            task = wait_task(api, key, report, output)
            require(task["attemptId"] == admitted["attemptId"] and task["runtime"] == admitted["runtime"],
                    "Admission references changed while executing")
            nonces.append(validate_result(task, project, vm, selected, run_id))
            require(task["attemptId"] not in attempts, "Separate tasks reused one attempt identity")
            attempts.add(task["attemptId"])
            require(runtime is None or runtime == task["runtime"], "MicroVM rebooted during the gate")
            runtime = task["runtime"]
            _, reloaded = api.request("GET", f"/api/ai-sandbox/tasks/{key}")
            require(reloaded == task, "Reload lost task, attempt, runtime or tool results")
            _, current_project = api.request("GET", f"/api/ai-sandbox/projects/{project}")
            require(current_project["spec"]["selection"] == default, "Task override mutated the project default")
            require((source / "guest-result.txt").read_text() == "not executed\n", "Execution changed the host source before apply")
            require(not source_git(source, "status", "--porcelain"), "Execution changed the source Git worktree")
            report["checks"][label] = "PASS"
            save_report(output, report)
        require(len(set(nonces)) == 2, "Separate attempts did not produce separate guest entropy")
        denied_model = f"unverified-{run_id}"
        status, _ = api.request("POST", f"/api/ai-sandbox/connections/{connection}/verify",
                                {"model": denied_model, "tools": True}, (400, 403, 404, 409, 422, 502))
        _, proofs = api.request("GET", f"/api/ai-sandbox/connections/{connection}/models")
        require(not any(m["id"] == denied_model and m.get("verified") for m in proofs["models"]),
                "Denied model gained an inference permission proof")
        report["checks"]["deniedModelVerification"] = {"status": "PASS", "httpStatus": status}
        for label, selection in (("deniedModel", {"connectionId": connection, "model": denied_model}),
                                 ("afterDisconnect", default)):
            if label == "afterDisconnect":
                # Recheck ownership immediately before the only credential mutation.
                _, connections = api.request("GET", "/api/pipe/ai/connections")
                require(any(c["id"] == connection and c["name"] == spec["name"] for c in connections), "Connection ownership changed")
                _, receipt = api.request("DELETE", f"/api/pipe/ai/connections/{connection}")
                require(receipt.get("deleted") is True, "QA connection deletion was not confirmed")
                report["connectionDisconnect"] = receipt
                api.request("GET", f"/api/ai-sandbox/connections/{connection}/models?discover=true", allowed=(404, 409))
                api.request("POST", f"/api/ai-sandbox/connections/{connection}/verify", {"model": args.default_model, "tools": True}, (404, 409))
            key = str(uuid.uuid4())
            status, _ = api.request("PUT", f"/api/ai-sandbox/projects/{project}/tasks/{key}",
                                    {"kind": "code", "input": "This request must never reach a provider or guest.", "selection": selection}, (400, 403, 409))
            api.request("GET", f"/api/ai-sandbox/tasks/{key}", allowed=(404,))
            report["checks"][label] = {"status": "PASS", "httpStatus": status, "taskId": key, "admitted": False}
        _, remaining = api.request("GET", "/api/pipe/ai/connections")
        require({c["id"] for c in remaining} == original_connections - {connection}, "Disconnect affected another account")
        # Export results before deleting only the project created above.
        report["checks"]["resultExport"] = "PASS"
        save_report(output, report)
        _, owned = api.request("GET", f"/api/ai-sandbox/projects/{project}")
        require(all(owned["spec"].get(k) == spec[k] for k in ("name", "sourcePath", "vmId")), "Project ownership changed; preserve it")
        api.request("DELETE", f"/api/ai-sandbox/projects/{project}")
        _, preserved = api.request("GET", f"/api/vms/{vm}")
        require(all(preserved.get(k) == machine[k] for k in ("id", "name", "state", "microNetworkId")), "Borrowed VM/network changed")
        report["checks"]["ownedCleanup"] = "PASS"
        report.update(status="PASS", retainedOnFailure=False)
        shutil.rmtree(source)
    except (Exception, KeyboardInterrupt) as error:
        report.update(status="FAILED", failure=f"{type(error).__name__}: {str(error)[:300]}")
        # Do not delete an uncertain attempt or its source. The report contains
        # stable IDs for inspect/cancel/reconcile, without a blind retry.
        raise
    finally:
        report["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save_report(output, report)
        print(f"{report['status']}: {output}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True)
    parser.add_argument("--run-id", required=True, type=uuid.UUID)
    parser.add_argument("--connection", required=True, type=uuid.UUID)
    parser.add_argument("--vm", required=True, type=uuid.UUID)
    parser.add_argument("--default-model", required=True)
    parser.add_argument("--override-model", required=True)
    parser.add_argument("--results-dir", required=True, type=Path)
    parser.add_argument("--live", action="store_true", help="Authorize real model requests and plan usage")
    parser.add_argument("--disconnect-owned-connection", action="store_true", help="Authorize disconnecting only this run's named QA connection")
    args = parser.parse_args()
    if sys.platform != "linux" or not args.live or not args.disconnect_owned_connection:
        parser.error("Requires Linux, --live, and --disconnect-owned-connection; use a separate QA account connection")
    try:
        run_gate(args, Api(args.api))
    except (Exception, KeyboardInterrupt) as error:
        print(f"FAILED: {type(error).__name__}: {str(error)[:300]}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
