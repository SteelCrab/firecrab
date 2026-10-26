#!/usr/bin/env python3
"""Opt-in real-model recovery acceptance on a dedicated disposable Linux API host.

Kills only the explicitly owned controller, reboots only the named QA MicroVM,
and exports evidence before cleanup. Never run against a shared API host.
"""

import argparse
import datetime
import hashlib
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import socket
import stat
import subprocess
import sys
import time
from urllib.parse import urlsplit
import uuid

_spec = importlib.util.spec_from_file_location("sandbox_foundation", Path(__file__).with_name("ci-qa-ai-sandbox.py"))
qa = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(qa)


def control(*args):
    result = subprocess.run(["/usr/bin/systemctl", *args], capture_output=True, timeout=30,
                            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"})
    qa.require(result.returncode == 0, "Owned controller operation failed; inspect before retrying")
    return result.stdout.decode().strip()


def owner_marker(path, run_id):
    parent = path.parent.lstat()
    qa.require(stat.S_ISDIR(parent.st_mode) and parent.st_uid == 0 and parent.st_mode & 0o022 == 0,
               "Ownership marker directory must be controlled by root")
    info = path.lstat()
    qa.require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_mode & 0o077 == 0,
               "The pre-created ownership marker must be a root-owned private regular file")
    qa.require(path.read_text().strip() == run_id, "This Linux host is not owned by the selected QA run")


def controller_pid(api, unit):
    value = control("show", "--property=MainPID", "--value", unit)
    qa.require(value.isdecimal() and int(value) > 1, "Owned controller is not running")
    process = Path(f"/proc/{value}")
    qa.require((process / "exe").resolve().name == "firecrab-api", "Controller executable does not match")
    sockets = {p.readlink().name for p in (process / "fd").iterdir() if p.is_symlink()}
    port = urlsplit(api.base).port
    listeners = [line.split() for line in Path("/proc/net/tcp").read_text().splitlines()[1:]]
    qa.require(any(row[1] == f"0100007F:{port:04X}" and row[3] == "0A" and f"socket:[{row[9]}]" in sockets
                   for row in listeners), "Selected API port is not served by the owned controller")
    return value


def binary_digest(pid):
    with open(f"/proc/{pid}/exe", "rb") as executable:
        return hashlib.file_digest(executable, "sha256").hexdigest()


def owned(api, run_id, connection, vm):
    label = f"sandbox-recovery-{run_id}"
    _, accounts = api.request("GET", "/api/pipe/ai/connections")
    qa.require(len(accounts) == 1 and accounts[0]["id"] == connection and accounts[0]["name"] == label,
               "Recovery requires a dedicated API with only this run's QA account")
    _, machines = api.request("GET", "/api/vms")
    qa.require(len(machines) == 1 and machines[0]["id"] == vm and machines[0]["name"] == label
               and machines[0]["state"] == "running" and machines[0].get("purpose") == "instance"
               and machines[0].get("egressPolicy") == "isolated",
               "Recovery requires exactly one running disposable user VM owned by this run")
    machine = machines[0]
    _, network = api.request("GET", f"/api/micro-networks/{machine['microNetworkId']}")
    qa.require(network["name"] == label, "VM network ownership does not match this run")
    qa.require(network.get("internetEnabled") is False and not network.get("ipv6Cidr"),
               "Recovery VM must have isolated IPv4 and no IPv6 network")
    _, projects = api.request("GET", "/api/ai-sandbox/projects")
    qa.require(not projects, "A shared API with existing projects cannot be crashed by this gate")
    return label, machine, network


class Guest:
    def __init__(self, machine, vms_dir, results):
        self.ip = str(ipaddress.IPv4Address(machine["ipv4"]))
        keys = vms_dir.resolve() / uuid.UUID(machine["id"]).hex / "ssh"
        self.key = keys / "id_ed25519"
        info = self.key.lstat()
        qa.require(stat.S_ISREG(info.st_mode) and info.st_mode & 0o077 == 0, "Expected private VM operator key")
        public = (keys / "ssh_host_ed25519_key.pub").read_text().split()
        qa.require(len(public) >= 2 and public[0] == "ssh-ed25519" and re.fullmatch(r"[A-Za-z0-9+/=]+", public[1]),
                   "Expected pinned VM host key")
        self.known = results / "known_hosts"
        self.known.write_text(f"{self.ip} {public[0]} {public[1]}\n")
        self.known.chmod(0o600)

    def run(self, script, allowed=(0,)):
        result = subprocess.run([
            "/usr/bin/ssh", "-F", "/dev/null", "-T", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none", "-o", "ForwardAgent=no", "-o", "ClearAllForwardings=yes",
            "-o", "StrictHostKeyChecking=yes", "-o", "GlobalKnownHostsFile=/dev/null",
            "-o", f"UserKnownHostsFile={self.known}", "-o", "ConnectTimeout=3",
            "-o", "ServerAliveInterval=3", "-o", "ServerAliveCountMax=2",
            "-i", str(self.key), f"root@{self.ip}", "/bin/sh -s"],
            input=script.encode(), capture_output=True, timeout=20, env={"PATH": "/usr/bin:/bin"})
        qa.require(result.returncode in allowed and len(result.stdout) <= 65536,
                   "Pinned operator SSH could not confirm the QA condition")
        return result.returncode, result.stdout.decode()

    def started(self, task):
        base = f"/var/lib/firecrab-ai/{uuid.UUID(task['attemptId']).hex}"
        code, text = self.run(f"test -f {base}/workspace/started || exit 1\n"
                              f"for file in {base}/workspace/nonce {base}/control/current {base}/control/boot; do cat \"$file\"; printf '\\n'; done\n", (0, 1))
        if code:
            return None
        fields = text.splitlines()
        qa.require(len(fields) == 3 and re.fullmatch(r"[0-9a-f]{32}", fields[0]), "Invalid guest execution evidence")
        return {"nonce": fields[0], "invocation": str(uuid.UUID(fields[1])), "boot": str(uuid.UUID(fields[2]))}


def command(run_id, label, delay):
    return f"""set -eu
test "$(id -u)" -ne 0
test "$(awk '/^NoNewPrivs:/ {{print $2}}' /proc/self/status)" = 1
test -z "${{OPENAI_API_KEY-}}${{ANTHROPIC_API_KEY-}}${{SSH_AUTH_SOCK-}}"
test ! -r /var/lib/firecrab-qa-retained/{run_id}/keep
nonce=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \\n')
printf '%s' "$nonce" > nonce
printf 'once\\n' >> executions.txt
printf 'RECOVERY414:{run_id}:{label}:stdout:%s\\n' "$nonce"
printf 'RECOVERY414:{run_id}:{label}:stderr\\n' >&2
touch started
sleep {delay}
exit 7"""


def inspect(api, task_id, report, output):
    _, task = api.request("GET", f"/api/ai-sandbox/tasks/{task_id}", timeout=10)
    report["tasks"][task_id] = task
    qa.save_report(output, report)
    return task


def wait(api, task_id, report, output, condition, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        task = inspect(api, task_id, report, output)
        if condition(task):
            return task
        time.sleep(0.5)
    raise qa.QaFailure(f"Recovery deadline exceeded; inspect task {task_id} before retrying")


def disconnected_put(api, path, request):
    # Send once, then inspect the known identity. Never repeat an uncertain PUT.
    body = json.dumps(request).encode()
    parsed = urlsplit(api.base)
    with socket.create_connection((parsed.hostname, parsed.port), timeout=10) as client:
        client.sendall((f"PUT {path} HTTP/1.1\r\nHost: {parsed.netloc}\r\nContent-Type: application/json\r\n"
                        f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n").encode() + body)
        client.shutdown(socket.SHUT_WR)


def validate(task, project, vm, selected, script, run_id, label, state, nonce=None):
    qa.require(task["projectId"] == project and task["selection"] == selected
               and task["runtime"]["vmId"] == vm and task["state"] == state, "Recovery changed task references or state")
    events = task.get("events", [])
    qa.require(len(events) == 1 and events[0]["name"] == "run_command"
               and json.loads(events[0]["input"])["command"] == script, "The authorized command was changed or replayed")
    event = events[0]
    match = re.fullmatch(rf"RECOVERY414:{re.escape(run_id)}:{label}:stdout:([0-9a-f]{{32}})\n", event["output"])
    qa.require(match is not None and (nonce is None or nonce == match.group(1)), "Recovered guest stdout does not match actual execution")
    qa.require(event["stderr"].startswith(f"RECOVERY414:{run_id}:{label}:stderr\n")
               and len(event["stderr"].encode()) <= 65536, "Recovered stderr was lost or unbounded")
    changes = {c["path"]: c for c in task["changes"]}
    qa.require(changes.get("executions.txt", {}).get("after") == "once\n"
               and changes.get("nonce", {}).get("after") == match.group(1)
               and not any(c.get("applied") for c in changes.values()), "Execution replayed or lost retained changes")
    return match.group(1)


def run_gate(args, api):
    run_id, connection, vm = map(str, (args.run_id, args.connection, args.vm))
    owner_marker(args.owner_marker, run_id)
    label, machine, network = owned(api, run_id, connection, vm)
    initial_pid = controller_pid(api, args.controller_unit)
    executable_digest = binary_digest(initial_pid)
    results = args.results_dir.resolve()
    results.mkdir(mode=0o700, parents=True, exist_ok=False)
    output = results / "summary.json"
    guest = Guest(machine, args.vms_dir, results)
    source = results / "source"
    source.mkdir(mode=0o755)
    (source / "executions.txt").write_text("")
    qa.source_git(source, "init", "-q")
    qa.source_git(source, "add", "executions.txt")
    qa.source_git(source, "commit", "-q", "--no-gpg-sign", "-m", "Recovery QA baseline")
    # The API service must read its own fixture, even when the operator is root.
    service_user = control("show", "--property=User", "--value", args.controller_unit) or "root"
    os.chown(results, pwd.getpwnam(service_user).pw_uid, -1)
    (source / "executions.txt").chmod(0o644)
    project = str(uuid.uuid4())
    selected = {"connectionId": connection, "model": args.model}
    spec = {"name": label, "sourcePath": str(source), "vmId": vm, "selection": None,
            "instructions": "Run only the exact authorized QA command once; never retry a nonzero exit."}
    report = {"runId": run_id, "status": "RUNNING", "projectId": project, "vmId": vm,
              "networkId": machine["microNetworkId"], "sourcePath": str(source), "checks": {}, "tasks": {}, "retainedOnFailure": True,
              "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(), "apiSha256": executable_digest,
              "harnessSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    qa.save_report(output, report)
    try:
        retained = f"/var/lib/firecrab-qa-retained/{run_id}"
        _, retained_nonce = guest.run(f"set -eu\numask 077\nmkdir -p {retained}\n"
                                      f"od -An -N16 -tx1 /dev/urandom | tr -d ' \\n' >{retained}/keep\ncat {retained}/keep\n")
        qa.require(re.fullmatch(r"[0-9a-f]{32}", retained_nonce) is not None, "Cannot establish retained guest data")
        report["retainedDataNonce"] = retained_nonce
        api.request("PUT", f"/api/ai-sandbox/projects/{project}", spec)
        report["checks"]["sourceVisibleToApi"] = "PASS"
        _, proof = api.request("POST", f"/api/ai-sandbox/connections/{connection}/verify", {"model": args.model, "tools": True})
        qa.require(proof.get("verified") and proof.get("toolsVerified"), "Real model tool permission is unverified")
        report["checks"]["nativeModelPermission"] = "PASS"
        spec["selection"] = selected
        api.request("PUT", f"/api/ai-sandbox/projects/{project}", spec)
        attempts = set()
        for scenario in ("timeout", "cancel", "reconnect", "controllerCrash", "guestReboot"):
            task_id = str(uuid.uuid4())
            script = command(run_id, scenario, 0 if scenario == "reconnect" else 90)
            limit = 2 if scenario == "timeout" else 120
            request = {"kind": "code", "input": f"Call run_command exactly once with timeout_seconds={limit}. Do not change this command, use other tools, or retry a nonzero exit. After receiving the result, reply with stdout, stderr and exit status.\n```sh\n{script}\n```"}
            report["tasks"][task_id] = {"id": task_id, "request": request}
            qa.save_report(output, report)
            path = f"/api/ai-sandbox/projects/{project}/tasks/{task_id}"
            if scenario == "reconnect":
                disconnected_put(api, path, request)
                deadline = time.monotonic() + 15
                while True:
                    status, _ = api.request("GET", f"/api/ai-sandbox/tasks/{task_id}", allowed=(200, 404), timeout=10)
                    if status == 200:
                        break
                    qa.require(time.monotonic() < deadline, "Disconnected admission remains uncertain; do not resubmit")
                    time.sleep(0.2)
            else:
                api.request("PUT", path, request, (200, 202))
            if scenario in ("timeout", "reconnect"):
                task = wait(api, task_id, report, output, lambda t: t["state"] not in ("waiting", "accepted", "in_progress"))
                nonce = validate(task, project, vm, selected, script, run_id, scenario, "result")
                qa.require(nonce in task["result"], "Model did not receive the guest result")
                qa.require(task["events"][0]["exitCode"] == 7 if scenario == "reconnect" else task["events"][0]["exitCode"] not in (None, 0, 7), "Unexpected real guest timeout/exit result")
            else:
                evidence = {}
                def started(task):
                    qa.require(task["state"] in ("waiting", "accepted", "in_progress"), "Task settled before fault injection")
                    current = guest.started(task)
                    if current:
                        evidence.update(current)
                    return bool(current)
                admitted = wait(api, task_id, report, output, started)
                if scenario == "cancel":
                    api.request("POST", f"/api/ai-sandbox/tasks/{task_id}/cancel", {}, (200, 202))
                elif scenario == "controllerCrash":
                    owner_marker(args.owner_marker, run_id)
                    before = controller_pid(api, args.controller_unit)
                    control("kill", "--kill-whom=main", "--signal=SIGKILL", args.controller_unit)
                    deadline = time.monotonic() + 60
                    while True:
                        try:
                            task = inspect(api, task_id, report, output)
                            after = control("show", "--property=MainPID", "--value", args.controller_unit)
                            if after != before and int(after) > 1:
                                break
                        except OSError:
                            pass
                        qa.require(time.monotonic() < deadline, "Controller did not recover; retain the recorded attempt")
                        time.sleep(0.5)
                    qa.require(task["state"] == "interrupted", "Restart automatically replayed or settled the uncertain attempt")
                    qa.require(binary_digest(after) == executable_digest, "Controller executable changed during recovery")
                    report["controllerPids"] = {"before": before, "after": after}
                else:
                    base = f"/var/lib/firecrab-ai/{uuid.UUID(admitted['attemptId']).hex}"
                    _, supervisor = guest.run(f"cat {base}/control/supervisor")
                    report["originalSupervisor"] = supervisor.strip()
                    guest.run("sync")
                    report["preRebootSync"] = True
                    api.request("POST", f"/api/vms/{vm}/stop", {}, (200, 202))
                    api.request("POST", f"/api/vms/{vm}/start", {}, (200, 202))
                    deadline = time.monotonic() + 90
                    while True:
                        _, current = api.request("GET", f"/api/vms/{vm}", timeout=10)
                        qa.require(current["ipv4"] == guest.ip, "Reboot changed the expected VM address")
                        if current["state"] == "running":
                            code, boot = guest.run("cat /proc/sys/kernel/random/boot_id", (0, 255))
                            if code == 0:
                                break
                        qa.require(time.monotonic() < deadline, "Owned VM did not restart; retain its evidence")
                        time.sleep(0.5)
                    qa.require(boot.strip() != evidence["boot"], "Guest boot identity did not change")
                    report["guestBoots"] = {"before": evidence["boot"], "after": boot.strip()}
                    # Reproduce PID reuse after a real reboot. The old boot fence must win
                    # even when a different root process matches the saved PID/start tuple.
                    _, reused = guest.run(f"set -eu\n/bin/sleep 300 </dev/null >/dev/null 2>&1 &\n"
                                          "pid=$!\nstart=$(awk '{print $22}' /proc/$pid/stat)\n"
                                          f"printf '%s %s\\n' \"$pid\" \"$start\" >{base}/control/supervisor\n"
                                          "printf '%s %s' \"$pid\" \"$start\"\n")
                    qa.require(re.fullmatch(r"[0-9]+ [0-9]+", reused) is not None, "Cannot establish PID-reuse fault")
                    report["simulatedReusedSupervisor"] = reused
                    wait(api, task_id, report, output, lambda t: t["state"] == "interrupted")
                if scenario != "cancel":
                    time.sleep(2)
                    same = inspect(api, task_id, report, output)
                    qa.require(same["state"] == "interrupted" and same["attemptId"] == admitted["attemptId"], "Uncertain attempt was automatically replayed")
                    api.request("POST", f"/api/ai-sandbox/tasks/{task_id}/reconcile", {})
                task = wait(api, task_id, report, output, lambda t: t["state"] == "cancelled")
                validate(task, project, vm, selected, script, run_id, scenario, "cancelled", evidence["nonce"])
                qa.require(task["attemptId"] == admitted["attemptId"] and task["runtime"] == admitted["runtime"], "Recovery rewrote original execution references")
                if scenario != "guestReboot":
                    qa.require(task["events"][0]["exitCode"] not in (None, 0), "Confirmed cancellation lost its exit status")
                if scenario != "cancel":
                    qa.require(task["errorCode"] is None, "Successful reconciliation retained a stale failure code")
                if scenario == "guestReboot":
                    pid, start = report["simulatedReusedSupervisor"].split()
                    guest.run(f"set -eu\nkill -0 {pid}\ntest \"$(awk '{{print $22}}' /proc/{pid}/stat)\" = {start}\nkill {pid}\n")
                    _, kept = guest.run(f"cat {retained}/keep")
                    qa.require(kept == retained_nonce, "Reboot or reconciliation deleted retained data")
                    report["checks"]["oldSupervisorNotSignalled"] = "PASS"
                report["checks"][scenario + "Evidence"] = evidence
                if scenario == "controllerCrash":
                    retry_id = str(uuid.uuid4())
                    retry_script = command(run_id, "explicitRetry", 0)
                    retry_request = {"kind": "code", "retryOf": task_id,
                                     "input": "Call run_command exactly once with timeout_seconds=30. Use no other tools and do not retry exit 7. Return the actual stdout nonce, stderr and exit status.\n```sh\n" + retry_script + "\n```"}
                    report["tasks"][retry_id] = {"id": retry_id, "request": retry_request}
                    qa.save_report(output, report)
                    api.request("PUT", f"/api/ai-sandbox/projects/{project}/tasks/{retry_id}", retry_request, (200, 202))
                    retried = wait(api, retry_id, report, output, lambda t: t["state"] not in ("waiting", "accepted", "in_progress"))
                    nonce = validate(retried, project, vm, selected, retry_script, run_id, "explicitRetry", "result")
                    qa.require(retried["attemptId"] != task["attemptId"] and retried["runtime"] == task["runtime"]
                               and nonce in retried["result"] and retried["request"]["retryOf"] == task_id, "Explicit retry lost its identity or guest result")
                    attempts.add(retried["attemptId"])
                    report["checks"]["explicitRetry"] = "PASS"
            qa.require(task["attemptId"] not in attempts, "Recovery reused an earlier attempt")
            attempts.add(task["attemptId"])
            qa.require((source / "executions.txt").read_text() == "" and not qa.source_git(source, "status", "--porcelain"), "Recovery changed the original source")
            report["checks"][scenario] = "PASS"
            qa.save_report(output, report)
        # Save all results before removing owned application data.
        _, current = api.request("GET", f"/api/ai-sandbox/projects/{project}")
        qa.require(current["spec"] == spec, "Project ownership changed; preserve its data")
        report["checks"]["resultExport"] = "PASS"
        qa.save_report(output, report)
        api.request("DELETE", f"/api/ai-sandbox/projects/{project}")
        _, preserved = api.request("GET", f"/api/vms/{vm}")
        _, preserved_network = api.request("GET", f"/api/micro-networks/{machine['microNetworkId']}")
        qa.require(preserved["name"] == label and preserved["state"] == "running"
                   and preserved["microNetworkId"] == machine["microNetworkId"] and preserved_network["id"] == network["id"], "Cleanup changed borrowed resources")
        _, kept = guest.run(f"cat {retained}/keep")
        qa.require(kept == retained_nonce, "Owned task cleanup removed retained guest data")
        report["checks"]["retainedDataPreserved"] = "PASS"
        _, accounts = api.request("GET", "/api/pipe/ai/connections")
        qa.require(len(accounts) == 1 and accounts[0]["id"] == connection and accounts[0]["name"] == label, "Account ownership changed")
        _, receipt = api.request("DELETE", f"/api/pipe/ai/connections/{connection}")
        report["connectionDisconnect"] = receipt
        qa.require(receipt.get("deleted") and receipt.get("remoteRevocationConfirmed"), "QA grant retirement is not confirmed")
        report["checks"]["borrowedResourcesPreserved"] = "PASS"
        report.update(status="PASS", retainedOnFailure=False)
        shutil.rmtree(source)
    except (Exception, KeyboardInterrupt) as error:
        report.update(status="FAILED", failure=f"{type(error).__name__}: {str(error)[:250]}")
        raise
    finally:
        report["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        qa.save_report(output, report)
        print(f"{report['status']}: {output}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("run-id", "connection", "vm"):
        parser.add_argument("--" + name, required=True, type=uuid.UUID)
    parser.add_argument("--api", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--controller-unit", default="firecrab-api.service", choices=("firecrab-api.service",))
    for name in ("owner-marker", "vms-dir", "results-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--allow-owned-faults", action="store_true")
    args = parser.parse_args()
    if sys.platform != "linux" or os.geteuid() != 0 or not args.live or not args.allow_owned_faults:
        parser.error("Requires root on an owned disposable Linux API host, --live and --allow-owned-faults")
    try:
        run_gate(args, qa.Api(args.api))
    except (Exception, KeyboardInterrupt) as error:
        print(f"FAILED: {type(error).__name__}: {str(error)[:250]}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
