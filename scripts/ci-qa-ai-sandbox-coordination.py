#!/usr/bin/env python3
"""Opt-in real Debug → Test acceptance. Never logs in or reads provider tokens."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET
from urllib.request import Request

_spec = importlib.util.spec_from_file_location("recovery", Path(__file__).with_name("ci-qa-ai-sandbox-recovery.py"))
recovery = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(recovery)
qa = recovery.qa


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


APPLICATION = "src/main/java/io/firecrab/qa/Application.java"


def application():
    return (Path(__file__).parent / "fixtures/sandbox-spring-boot" / APPLICATION).read_text()


def fixed_application():
    return application().replace("    public static void main", '    @org.springframework.context.annotation.Bean\n    String greeting() { return "ok"; }\n\n    public static void main', 1)


def save(path, report):
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as output:
        os.fchmod(output.fileno(), 0o600)
        json.dump(report, output, indent=2)
        output.write("\n")
    temporary.replace(path)


def command(label):
    # Tooling/cache are provisioned by the operator, outside the project and task export.
    # The task runs offline; only its own writable source copy changes.
    return f"""set -u
test "$(id -u)" -ne 0 || exit 86
test "$(awk '/^NoNewPrivs:/ {{print $2}}' /proc/self/status)" = 1 || exit 86
test -z "${{OPENAI_API_KEY-}}${{ANTHROPIC_API_KEY-}}${{SSH_AUTH_SOCK-}}" || exit 86
mkdir -p qa
test ! -e qa/{label}.executed || exit 87
nonce=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \\n')
printf '%s\\n' "$nonce" >qa/{label}.executed
/opt/firecrab/qa/maven/bin/mvn -o -q -Dmaven.repo.local=/opt/firecrab/qa/m2 '-DargLine=-Xmx128m -XX:+UseSerialGC -XX:MaxMetaspaceSize=96m -XX:CompressedClassSpaceSize=32m -XX:ReservedCodeCacheSize=32m -Xss256k' test >qa/{label}.log 2>&1
code=$?
printf 'Maven log bytes: '; wc -c <qa/{label}.log
printf 'Last 24576 log bytes follow; the full log remains in qa/{label}.log\\n'
tail -c 24576 qa/{label}.log
test ! -f target/surefire-reports/TEST-io.firecrab.qa.GreetingTest.xml || cp target/surefire-reports/TEST-io.firecrab.qa.GreetingTest.xml qa/{label}.xml
printf 'FIRECRAB414:{label}:stdout:%s\\n' "$nonce"
printf 'FIRECRAB414:{label}:exit:%s\\n' "$code" >&2
exit "$code"
"""


def invocation(label):
    return {"version": 1, "argv": ["/bin/sh", "-c", command(label)], "cwd": "", "stdin": "",
            "env": [{"name": "JAVA_HOME", "value": "/opt/firecrab/qa/java"},
                    {"name": "MAVEN_OPTS", "value": "-Xmx128m -XX:+UseSerialGC -XX:MaxMetaspaceSize=96m -XX:CompressedClassSpaceSize=32m -XX:ReservedCodeCacheSize=32m -Xss256k"}],
            "limits": {"wallSeconds": 120, "cpuSeconds": 120, "memoryMib": 2048, "outputBytes": 65536}}


def input_for(agent, dependency=None):
    if agent == "Debug":
        text = ("Inspect Application.java and GreetingController.java. Reproduce the missing String bean failure by calling run_argv ONCE with exactly this JSON: "
                + json.dumps(invocation("debug-failure"))
                + " . Keep its actual missing-bean failure and exit 1 as command evidence. The regression test is recorded after the fix. "
                  "Use write_file to replace only src/main/java/io/firecrab/qa/Application.java with exactly this UTF-8 content: "
                + json.dumps(fixed_application()) + " . "
                  "Then call run_argv ONCE with exactly this JSON: " + json.dumps(invocation("debug-fixed"))
                + " . Record the actual successful command as debug-regression. Retain qa/debug-fixed.xml and qa/debug-fixed.executed with retain_artifact. "
                  "Do not repeat either command. Finish with the actual failure, fix, test result and unpredictable guest nonce.")
    else:
        text = ("You are Test. The successful Debug dependency must supply its retained diff and result. Inspect the inherited Application.java to confirm the String bean fix. "
                "Run your own independent regression ONCE using run_argv with exactly this JSON: " + json.dumps(invocation("test-independent"))
                + " . Record the actual completed command using record_test name=test-independent-regression and its eventSequence. "
                  "Retain qa/test-independent.xml and qa/test-independent.executed. Do not change source or repeat a command. "
                  "Finish with the actual test result and unpredictable guest nonce.")
    return {"kind": "code", "input": text, "selection": None, "retryOf": None,
            "coordination": {"assignedAgent": agent, "dependencies": [] if dependency is None else [dependency], "retryLimit": 1}}


def commands(task):
    return [event for event in task["events"] if event["name"] in ("run_command", "run_argv")]


def validate(task, agent, labels, codes):
    qa.require(task["state"] == "result", f"{agent} did not reach Result: {task['state']} / {task.get('errorCode')}")
    events = commands(task)
    qa.require(len(events) == len(labels), f"{agent} executed an unexpected number of commands")
    for event, label, code in zip(events, labels, codes):
        qa.require(event["name"] == "run_argv" and json.loads(event["input"]) == invocation(label), "The model changed the bounded QA invocation")
        qa.require(event["exitCode"] == code and f"FIRECRAB414:{label}:stdout:" in event["output"]
                   and f"FIRECRAB414:{label}:exit:{code}\n" in event["stderr"], "Actual command output/exit evidence is missing")
        nonce = re.search(rf"FIRECRAB414:{re.escape(label)}:stdout:([0-9a-f]{{32}})\n", event["output"])
        qa.require(nonce is not None, "Missing unpredictable guest nonce")
        if code == 0:
            qa.require(nonce.group(1) in task.get("result", ""), "Model did not return the successful guest nonce")
    tests = task["tests"]
    qa.require(len(tests) == codes.count(0), "Missing command-backed regression test records")
    for event in events:
        if event["exitCode"] == 0:
            qa.require(any(test["eventSequence"] == event["sequence"] and test["passed"] for test in tests), "Regression record disagrees with the actual guest command")
    prefix = "debug-fixed" if agent == "Debug" else "test-independent"
    paths = [artifact.get("path") for artifact in task["artifacts"]]
    required = {f"qa/{prefix}.xml", f"qa/{prefix}.executed"}
    diagnostics = {APPLICATION} | {f"qa/{label}.log" for label in labels}
    qa.require(required.issubset(paths), "Missing XML/nonce artifact export")
    qa.require(len(set(paths)) == len(paths) and set(paths).issubset(required | diagnostics),
               "Duplicate or unrelated artifacts cannot establish regression evidence")
    for event in task["events"]:
        if event["name"] in ("write_file", "delete_file"):
            arguments = json.loads(event["input"])
            qa.require(agent == "Debug" and event["name"] == "write_file" and arguments == {"path": APPLICATION, "content": fixed_application()}, "The model changed unauthorized source")
    allowed = {APPLICATION} | {f"qa/{label}.{suffix}" for label in ["debug-failure", "debug-fixed", "test-independent"] for suffix in ["log", "xml", "executed"]}
    qa.require(all(change["path"] in allowed for change in task["changes"]), "The build, tests, instructions or other source changed")
    application_changes = [change for change in task["changes"] if change["path"] == APPLICATION]
    qa.require(len(application_changes) == 1 and application_changes[0]["before"] == application()
               and application_changes[0]["after"] == fixed_application() and not application_changes[0]["applied"], "Expected only the exact known Application fix")
    if agent == "Debug":
        qa.require("java.lang.String" in events[0]["output"], "Spring Boot missing-bean failure was not reproduced")
        qa.require(any(change["path"].endswith("Application.java") and "@org.springframework.context.annotation.Bean" in (change.get("after") or "") for change in task["changes"]), "Known fix was not retained as a reviewed change")


def wait(api, key, report, output):
    deadline = time.monotonic() + 620
    while time.monotonic() < deadline:
        _, task = api.request("GET", f"/api/ai-sandbox/tasks/{key}", timeout=10)
        report["tasks"][key] = task
        save(output, report)
        if task["state"] not in ("waiting", "accepted", "in_progress"):
            return task
        time.sleep(2)
    raise qa.QaFailure("Task wait deadline expired; inspect the durable attempt without resubmitting")


def download(api, task, results, prefix):
    event = next(event for event in commands(task) if json.loads(event["input"]) == invocation(prefix))
    nonce = re.search(rf"FIRECRAB414:{re.escape(prefix)}:stdout:([0-9a-f]{{32}})\n", event["output"]).group(1)
    for artifact in task["artifacts"]:
        qa.require(artifact["taskId"] == task["id"] and artifact["attemptId"] == task["attemptId"], "Artifact ownership changed")
        request = Request(api.base + f"/api/ai-sandbox/tasks/{task['id']}/artifacts/{artifact['id']}")
        with api.http.open(request, timeout=10) as response:
            data = response.read(1024 * 1024 + 1)
        qa.require(len(data) == artifact["size"] and hashlib.sha256(data).hexdigest() == artifact["sha256"], "Downloaded artifact differs from durable metadata")
        path = results / f"{artifact['id']}.artifact"
        path.write_bytes(data)
        path.chmod(0o600)
        if artifact["path"].endswith(".xml"):
            suite = ET.fromstring(data)
            cases = suite.findall("testcase")
            qa.require(suite.tag == "testsuite" and suite.get("tests") == "1" and suite.get("errors") == "0" and suite.get("failures") == "0"
                       and len(cases) == 1 and cases[0].get("classname") == "io.firecrab.qa.GreetingTest"
                       and cases[0].get("name") == "servesConfiguredGreeting" and cases[0].find("failure") is None
                       and cases[0].find("error") is None, "Actual intended JUnit regression did not pass")
        elif artifact["path"] == f"qa/{prefix}.executed":
            qa.require(data == (nonce + "\n").encode(), "Nonce artifact does not match the actual command")
        else:
            change = next((change for change in task["changes"] if change["path"] == artifact["path"]), None)
            qa.require(change is not None and isinstance(change.get("after"), str)
                       and data == change["after"].encode(), "Diagnostic artifact differs from the retained source/log change")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["api", "run-id", "connection", "vm", "model", "owner-marker", "fixture", "results-dir"]:
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    qa.require(args.live and sys.platform == "linux" and os.geteuid() == 0, "Explicit --live on the owned Linux API host is required")
    run_id = str(uuid.UUID(args.run_id))
    recovery.owner_marker(Path(args.owner_marker), run_id)
    api = qa.Api(args.api)
    pid = recovery.controller_pid(api, "firecrab-api.service")
    _, machine = api.request("GET", f"/api/vms/{uuid.UUID(args.vm)}", timeout=10)
    qa.require(machine["name"] == f"sandbox-recovery-{run_id}" and machine["state"] == "running" and machine["egressPolicy"] == "isolated", "Expected the retained isolated QA VM")
    _, accounts = api.request("GET", "/api/pipe/ai/connections", timeout=10)
    qa.require(any(account["id"] == args.connection and account["name"] == machine["name"] for account in accounts), "Expected the same human-authorized QA connection")
    _, projects = api.request("GET", "/api/ai-sandbox/projects", timeout=10)
    qa.require(not any(project["spec"].get("vmId") == args.vm for project in projects),
               "Export and explicitly clean up the previous owned project before borrowing this VM again")
    fixture = Path(args.fixture).resolve()
    qa.require(fixture.is_dir() and not fixture.is_symlink(), "Expected the checked-in Spring Boot fixture")
    results = Path(args.results_dir)
    results.mkdir(mode=0o755, parents=False, exist_ok=False)
    source = results / "source"
    shutil.copytree(fixture, source)
    skill = source / "skills/qa/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("# Offline regression\nUse the prepared Java/Maven cache. Edit the isolated task copy. Firecrab preserves the API-host source. Record actual guest command results.\n")
    git_env = {"PATH": "/usr/bin:/bin", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0"}
    for git_args in [["init", "-q"], ["add", "."], ["-c", "user.name=Firecrab QA", "-c", "user.email=qa@localhost", "commit", "-qm", "seed offline regression"]]:
        subprocess.run(["/usr/bin/git", *git_args], cwd=source, env=git_env, capture_output=True, check=True)
    service_user = recovery.control("show", "--property=User", "--value", "firecrab-api.service")
    account = pwd.getpwnam(service_user or "root")
    for directory, directories, files in os.walk(source):
        os.chown(directory, account.pw_uid, account.pw_gid)
        for name in files:
            os.chown(Path(directory) / name, account.pw_uid, account.pw_gid)
    app = source / "src/main/java/io/firecrab/qa/Application.java"
    original = digest(app)
    source_hashes = {str(path.relative_to(source)): digest(path) for path in source.rglob("*")
                     if path.is_file() and ".git" not in path.relative_to(source).parts}
    output = results / "summary.json"
    project, debug, test = (str(uuid.uuid4()) for _ in range(3))
    report = {"runId": run_id, "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "status": "RUNNING", "projectId": project, "apiSha256": recovery.binary_digest(pid),
              "harnessSha256": digest(Path(__file__)), "tasks": {}, "checks": {},
              "connectionRetained": True, "resourcesBorrowed": {"vmId": args.vm, "networkId": machine["microNetworkId"]}}
    save(output, report)
    try:
        selection = {"connectionId": str(uuid.UUID(args.connection)), "model": args.model}
        spec = {"name": "Spring Boot Debug and Test", "sourcePath": str(source), "vmId": args.vm,
                "selection": selection, "instructions": "Use only the authorized offline fixture. Never request credentials, external network, or host execution.",
                "context": {"gitRef": "HEAD", "selectedSkills": ["skills/qa/SKILL.md"], "references": ["README.md"]}}
        api.request("PUT", f"/api/ai-sandbox/projects/{project}", spec, (200, 201), timeout=15)
        # Each identity is submitted once. An uncertain response is preserved for operator inspection.
        report["plannedTasks"] = {"Debug": debug, "Test": test}
        save(output, report)
        api.request("PUT", f"/api/ai-sandbox/projects/{project}/tasks/{debug}", input_for("Debug"), (200, 201, 202), timeout=15)
        _, queued = api.request("PUT", f"/api/ai-sandbox/projects/{project}/tasks/{test}", input_for("Test", debug), (200, 201, 202), timeout=15)
        qa.require(queued["state"] == "waiting" and not queued["events"], "Dependent Test did not wait before side effects")
        report["checks"]["dependentWaiting"] = "PASS"
        saved_debug = wait(api, debug, report, output)
        validate(saved_debug, "Debug", ["debug-failure", "debug-fixed"], [1, 0])
        report["checks"]["springBootFailureFixRegression"] = "PASS"
        saved_test = wait(api, test, report, output)
        validate(saved_test, "Test", ["test-independent"], [0])
        dependency = saved_test["dependencyResults"]
        qa.require(len(dependency) == 1 and dependency[0]["taskId"] == debug and dependency[0]["attemptId"] == saved_debug["attemptId"], "Test did not receive the successful Debug attempt")
        qa.require(saved_debug["runtime"] == saved_test["runtime"] and saved_debug["attemptId"] != saved_test["attemptId"], "Task/attempt/runtime identities are inconsistent")
        report["checks"]["debugToTestHandoff"] = "PASS"
        for task, prefix in [(saved_debug, "debug-fixed"), (saved_test, "test-independent")]:
            download(api, task, results, prefix)
            _, reloaded = api.request("GET", f"/api/ai-sandbox/tasks/{task['id']}", timeout=10)
            qa.require(reloaded["tests"] == task["tests"] and reloaded["artifacts"] == task["artifacts"], "Results changed after reload")
        qa.require(digest(app) == original, "Original source was modified")
        qa.require(source_hashes == {str(path.relative_to(source)): digest(path) for path in source.rglob("*")
                                    if path.is_file() and ".git" not in path.relative_to(source).parts}, "Original source files changed")
        report["checks"].update({"artifactDownload": "PASS", "reload": "PASS", "sourceUnchanged": "PASS"})
        report["status"] = "PASS"
        report["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save(output, report)
        print(f"PASS: {output}")
        # Keep the project/source/results for UI/restart verification and a later explicit owned cleanup.
    except Exception as error:
        report["status"] = "FAILED"
        report["error"] = str(error)
        save(output, report)
        print(f"FAILED: {output}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except qa.QaFailure as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
