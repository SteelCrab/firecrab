#!/usr/bin/env python3
"""Prepare an offline OpenRC V1 QA plan. There is no API, SSH, model or exec mode."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import uuid

PROJECT = "70b3dc57-c6e6-4033-8409-d00d7b406d30"
ENVIRONMENT = "4b9b2c97-bff6-4623-aa30-df7e9812c141"
CONNECTION = "b6804610-c932-40b3-9d37-3cebd426b8e8"
ROLE = "debug"
SELECTION = {"connectionId": CONNECTION, "model": "gpt-6.1-sol"}
CASES = ("literal", "memory", "wall", "output", "cancel")
LITERAL_ARGS = ["space argument", "single'quote", 'double"quote',
                "$HOME;$(printf no-shell);*?[]", "line\nbreak", ""]
LITERAL_ENV = "literal $HOME;$(printf no-shell)\nvalue"
STDIN = "public-input\x00bytes\n"


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def baseline(project, environment):
    """Freeze exported ownership/NoModel state without changing the supplied DTOs."""
    if (project.get("id") != PROJECT or environment.get("id") != ENVIRONMENT
            or environment.get("projectId") != PROJECT
            or project.get("owner") is None or project.get("owner") != environment.get("owner")
            or project["spec"].get("environmentId") != ENVIRONMENT
            or project["spec"].get("vmId") is not None
            or not re.search(r"no[\s_-]*model", project["spec"].get("instructions", ""), re.I)
            or environment.get("state") != "ready" or environment.get("dnsMode") != "private_only"):
        raise ValueError("Wrong ownership, missing NoModel baseline or non-ready Environment")
    generation = str(uuid.UUID(environment["generation"]))
    role = next((r for r in environment["roles"] if r.get("role") == ROLE), None)
    if (role is None or not role.get("runtime") or role["runtime"].get("vmId") != role.get("vmId")
            or role.get("agent") != "ready" or role.get("network") != "ready"):
        raise ValueError("No exact debug runtime in the exported baseline")
    if not uuid.UUID(generation).int or not all(uuid.UUID(role["runtime"][key]).int for key in ("vmId", "runtimeId")):
        raise ValueError("Nil generation/runtime is not evidence")
    return {"projectSpecSha256": digest(project["spec"]), "projectBefore": project,
            "environmentBeforeSha256": digest(environment), "runtime": role["runtime"],
            "environment": {"id": ENVIRONMENT, "generation": generation, "role": ROLE}}


def invocation(run_id, case):
    """Use the native tool's env ARRAY, not the internal REST DTO's env map."""
    return {"version": 1, "argv": ["python3", "probe.py", case, run_id.hex]
            + (LITERAL_ARGS if case == "literal" else []), "cwd": "qa cwd",
            "env": [{"name": "QA_V1_LITERAL", "value": LITERAL_ENV},
                    {"name": "QA_V1_RUN", "value": run_id.hex}],
            "stdin": STDIN if case == "literal" else "",
            "limits": {"wallSeconds": 2 if case == "wall" else 120 if case == "cancel-running" else 15,
                       "cpuSeconds": 3, "memoryMib": 64 if case == "memory" else 96,
                       "outputBytes": 256 if case == "output" else 4096}}


def task(run_id, case):
    commands = [invocation(run_id, value) for value in
                (("cancel-ready", "cancel-running") if case == "cancel" else (case,))]
    artifact = f"qa cwd/{run_id.hex}-{'cancel-ready' if case == 'cancel' else case}.json"
    prompt = ("Use only this approved public QA fixture. Do not edit files, install anything, access credentials, "
              "change the engine/OS/network, use run_command, replay a command or retry a failure. "
              "Each listed run_argv call is explicitly authorized exactly ONCE, in the listed order. "
              "Use these literal JSON inputs without changing argv, env, stdin, cwd or limits:\n"
              + json.dumps(commands, ensure_ascii=True) + "\n")
    if case == "cancel":
        prompt += ("After cancel-ready actually returns exit 0, record_test(name=openrc-cancel-ready) "
                   "using its actual tool-result eventSequence and retain_artifact for " + artifact
                   + ". Then make the second listed run_argv call once. Root will inspect and cancel it. "
                     "Do not mark cancellation complete from an acknowledgement. If it returns instead, "
                     "report its actual result; do not retry or claim cancellation. ")
    else:
        prompt += ("After the actual command returns, record_test(name=openrc-" + case
                   + ") using the actual tool-result eventSequence. Do not invent pass/fail; "
                     "the controller derives it from the exit status. Then retain_artifact for "
                   + artifact + ". ")
    prompt += ("Return the actual guest nonce, separate stdout/stderr and raw exit status. "
               "Literal intentionally exits 7. Do not normalize timeout, signal, memory or output exits. "
               "If execution is unsupported/uncertain, stop and retain the failure; never substitute a shell.")
    return {"id": str(uuid.uuid5(run_id, case)), "case": case, "invocations": commands,
            "artifactPath": artifact,
            "request": {"kind": "code", "input": prompt, "selection": dict(SELECTION),
                        "environmentRole": ROLE, "retryOf": None,
                        "coordination": {"assignedAgent": "OpenRC V1 QA", "dependencies": [], "retryLimit": 0}}}


def plan(run_id, frozen=None):
    return {"version": 1, "preparedOnly": True, "liveQaAccepted": False, "runId": str(run_id),
            "projectId": PROJECT, "environmentId": ENVIRONMENT, "role": ROLE,
            "selection": dict(SELECTION), "frozenBaseline": frozen,
            "remoteSourcePath": f"/srv/firecrab-qa/openrc-v1-414-{run_id.hex}/source",
            "projectMutationPrepared": False,
            "rootGates": {"strictPairBaselineRecoveredAndExported": False,
                          "coreDeployFaultRecoveryNetworkChecksComplete": False,
                          "originalNoModelSpecBackedUp": False,
                          "explicitSourceInstructionsSelectionReview": False,
                          "debugRuntimeAndGuestHealthOpenrcRechecked": False},
            "taskWaitSeconds": 180, "pollRequestTimeoutSeconds": 10,
            "uncertainAdmission": "GET the same known task UUID; never resend or choose a new UUID",
            "cleanup": "No automatic project/environment/connection/VM/network/seed/DB cleanup",
            "tasks": [task(run_id, case) for case in CASES]}


def admission(saved, planned, item, previous=None):
    """Validate an exported admission before trusting its immutable execution references."""
    frozen = planned.get("frozenBaseline")
    if not frozen:
        raise ValueError("Exported baseline is required before validating a live admission")
    if (saved.get("id") != item["id"] or saved.get("projectId") != PROJECT
            or saved.get("selection") != SELECTION or saved.get("request") != item["request"]
            or saved.get("environment") != frozen["environment"]
            or saved.get("runtime") != frozen["runtime"] or not uuid.UUID(saved["attemptId"]).int):
        raise ValueError("Task admission/ownership changed; retain and inspect without resubmitting")
    refs = {key: saved[key] for key in ("id", "projectId", "attemptId", "runtime", "environment")}
    if previous is not None and refs != previous:
        raise ValueError("Attempt/runtime changed after admission")
    return refs


def artifact(saved, planned, item, metadata, payload):
    """Check exported public bytes against their actual task/event/nonce ownership."""
    admission(saved, planned, item)
    if (saved.get("state") != ("cancelled" if item["case"] == "cancel" else "result")
            or metadata.get("taskId") != saved["id"] or metadata.get("attemptId") != saved["attemptId"]
            or metadata.get("path") != item["artifactPath"] or not uuid.UUID(metadata["id"]).int
            or metadata not in saved.get("artifacts", [])
            or len(payload) > 1024 * 1024 or metadata.get("size") != len(payload)
            or metadata.get("sha256") != hashlib.sha256(payload).hexdigest()
            or any(change.get("applied") or change.get("path") in ("probe.txt", "qa cwd/probe.py")
                   for change in saved.get("changes", []))
            or any(event.get("name") not in {"project_context", "run_argv", "record_test", "retain_artifact", "read_file", "list_files"}
                   for event in saved.get("events", []))):
        raise ValueError("Artifact ownership/bytes or terminal source state differ")
    value = json.loads(payload)
    label = "cancel-ready" if item["case"] == "cancel" else item["case"]
    nonce = value.get("nonce", "")
    memory = 64 if label == "memory" else 96
    if (value.get("runId") != uuid.UUID(planned["runId"]).hex or value.get("case") != label
            or not re.fullmatch(r"[0-9a-f]{32}", nonce)
            or value.get("uid", 0) == 0 or value.get("euid", 0) == 0
            or value.get("groups") != [] or value.get("noNewPrivs") != 1
            or int(value.get("capEff", "1"), 16) != 0 or value.get("cwdName") != "qa cwd"
            or value.get("pid", 0) <= 1 or value.get("startTicks", 0) <= 0
            or not uuid.UUID(value["bootId"]).int
            or value.get("addressSpaceLimit") != [memory * 1024 * 1024] * 2
            or value.get("cpuLimit") != [3, 3] or value.get("nofileLimit") != [1024, 1024]):
        raise ValueError("Public artifact nonce/privilege/limit contract differs")
    events = [event for event in saved.get("events", []) if event.get("name") in ("run_command", "run_argv")]
    if len(events) != len(item["invocations"]) or any(
            event["name"] != "run_argv" or json.loads(event["input"]) != request
            for event, request in zip(events, item["invocations"])):
        raise ValueError("A guest command changed or was repeated")
    first = events[0]
    prefix = f"OPENRCV1:{value['runId']}:{label}:stdout:{nonce}\n"
    stderr = f"OPENRCV1:{value['runId']}:{label}:stderr\n"
    if not first.get("output", "").startswith(prefix) or not first.get("stderr", "").startswith(stderr):
        raise ValueError("Actual event stdout/stderr lost the artifact nonce")
    if label == "literal":
        if (first.get("exitCode") != 7 or first["output"] != prefix or first["stderr"] != stderr
                or value.get("argv") != LITERAL_ARGS
                or value.get("env") != {"QA_V1_LITERAL": LITERAL_ENV, "QA_V1_RUN": value["runId"]}
                or value.get("stdinHex") != STDIN.encode().hex()
                or value.get("stdinSha256") != hashlib.sha256(STDIN.encode()).hexdigest()):
            raise ValueError("Literal argv/env/NUL stdin or deliberate exit 7 differs")
    elif label == "cancel-ready":
        if first.get("exitCode") != 0 or first["output"] != prefix or first["stderr"] != stderr:
            raise ValueError("Cancel preparation did not actually complete successfully")
    elif first.get("exitCode") in (None, 0):
        raise ValueError("Limit termination lacks an actual failing exit status")
    if label == "memory" and "MemoryError" not in first["stderr"]:
        raise ValueError("Memory evidence needs operator review; a signal alone is inconclusive")
    if any(len(event.get(stream, "").encode()) > request["limits"]["outputBytes"]
           for event, request in zip(events, item["invocations"]) for stream in ("output", "stderr")):
        raise ValueError("Retained output exceeded its actual requested per-stream limit")
    if item["case"] != "cancel" and nonce not in saved.get("result", ""):
        raise ValueError("Model result did not return the actual guest nonce")
    tests = saved.get("tests", [])
    if (len(tests) != 1 or tests[0].get("eventSequence") != first["sequence"]
            or tests[0].get("passed") is not (first["exitCode"] == 0)):
        raise ValueError("Recorded test does not match its actual command event")
    return {"artifactBytesVerified": True, "nonce": nonce,
            "rawExitCodes": [event.get("exitCode") for event in events],
            "operatorTerminationReceiptRequired": item["case"] in ("wall", "cancel"),
            "liveQaAccepted": False}


def prepare(output, run_id, frozen=None):
    """Write private review files only; no remote path is created by this program."""
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    source = output / "source" / "qa cwd"
    source.mkdir(mode=0o700, parents=True)
    payload = Path(__file__).parent / "fixtures/sandbox-openrc-v1/probe.py"
    (source / "probe.py").write_bytes(payload.read_bytes())
    (output / "source/probe.txt").write_text("not executed\n")
    planned = plan(run_id, frozen)
    for p in (source / "probe.py", output / "source/probe.txt"):
        p.chmod(0o600)
    for name, value in [("plan.json", planned)] + [(f"task-{item['case']}.json", item) for item in planned["tasks"]]:
        p = output / name
        p.write_text(json.dumps(value, indent=2, ensure_ascii=True) + "\n")
        p.chmod(0o600)
    return planned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--run-id", type=uuid.UUID, default=uuid.uuid4())
    parser.add_argument("--project-json", type=Path)
    parser.add_argument("--environment-json", type=Path)
    args = parser.parse_args()
    if not args.run_id.int or bool(args.project_json) != bool(args.environment_json):
        parser.error("Use a non-nil run UUID and either both exported baseline JSON files or neither")
    frozen = baseline(json.loads(args.project_json.read_text()), json.loads(args.environment_json.read_text())) if args.project_json else None
    prepare(args.output, args.run_id, frozen)
    print(json.dumps({"preparedOnly": True, "liveQaAccepted": False, "output": str(args.output)}))


if __name__ == "__main__":
    main()
