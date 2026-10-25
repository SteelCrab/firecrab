#!/usr/bin/env python3
"""Run isolated PipeSecrets QA on Linux; never connect to an installed API."""

import argparse
import datetime
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def run_stage(name, command, results, env):
    log = results / f"{name}.log"
    print(f"RUN {name}: {shlex.join(command)}", flush=True)
    with log.open("w") as output:
        try:
            process = subprocess.Popen(
                command, cwd=ROOT, env=env, stdout=output,
                stderr=subprocess.STDOUT, start_new_session=True,
            )
        except OSError as error:
            output.write(str(error))
            code = 127
        else:
            try:
                code = process.wait(timeout=900)
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                if isinstance(error, KeyboardInterrupt):
                    raise
                code = 124
    status = "PASS" if code == 0 else "FAILED"
    print(f"{status} {name} (exit {code}); {log}", flush=True)
    return {"name": name, "status": status, "command": command, "exitCode": code, "log": str(log)}


def browser_cases(report):
    if not report.exists():
        return {}
    document = json.loads(report.read_text())
    cases = {}

    def visit(suite):
        for spec in suite.get("specs", []):
            match = re.match(r"(SEC\d+):", spec["title"])
            if not match:
                continue
            statuses = [result["status"] for test in spec["tests"] for result in test["results"]]
            if not statuses or "skipped" in statuses:
                status = "WARNING"
            else:
                status = "PASS" if all(item == "passed" for item in statuses) else "FAILED"
            cases[match[1]] = {"status": status, "title": spec["title"], "report": str(report)}
        for child in suite.get("suites", []):
            visit(child)

    for suite in document.get("suites", []):
        visit(suite)
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results-dir", type=Path, help="New directory for this run's report and logs")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("Run in Linux or the managed Debian environment; a native macOS API build is not supported")
    for executable in ("git", "cargo", "node", "npm"):
        if shutil.which(executable) is None:
            parser.error(f"Required executable is missing: {executable}")
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    results = (args.results_dir or ROOT / "target/qa/secrets" / timestamp).resolve()
    results.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    target = Path(env.get("CARGO_TARGET_DIR", ROOT / "target"))
    env.setdefault("FIRECRAB_SECRETS_E2E_API_BIN", str((ROOT / target / "debug/firecrab-api").resolve()))
    report = {
        "revision": git("rev-parse", "HEAD"),
        "dirty": bool(git("status", "--porcelain")),
        "os": platform.platform(), "architecture": platform.machine(),
        "virtualization": "No guest boot; Chromium and two disposable Linux API processes",
        "startedAt": timestamp, "resultsDir": str(results), "stages": [],
    }
    stages = [
        ("api-build", ["cargo", "build", "-p", "firecrab-api", "--locked"]),
        ("api-tests", ["cargo", "test", "-p", "firecrab-api", "secrets::", "--locked"]),
        ("frontend-lint", ["npm", "run", "lint", "--prefix", "firecrab-frontend"]),
        ("frontend-build", ["npm", "run", "build", "--prefix", "firecrab-frontend"]),
        ("browser-types", ["npm", "run", "typecheck:secrets", "--prefix", "firecrab-e2e"]),
        ("secrets-ui", ["npm", "run", "test:secrets", "--prefix", "firecrab-e2e"]),
        ("secrets-live", ["npm", "run", "test:secrets:live", "--prefix", "firecrab-e2e"]),
    ]
    cases = {}
    for name, command in stages:
        browser_report = ROOT / f"firecrab-e2e/test-results/{name}/results.json"
        if name in ("secrets-ui", "secrets-live"):
            browser_report.unlink(missing_ok=True)
        # Do not treat an old executable or dashboard build as this run's evidence.
        build_failed = any(stage["status"] != "PASS" for stage in report["stages"] if stage["name"] in ("api-build", "frontend-build"))
        if name == "secrets-live" and build_failed:
            report["stages"].append({"name": name, "status": "WARNING", "reason": "Required build failed"})
            continue
        stage = run_stage(name, command, results, env)
        report["stages"].append(stage)
        if name in ("secrets-ui", "secrets-live"):
            # Copy reports before another Playwright run can replace them.
            if browser_report.exists():
                saved_report = results / f"{name}.json"
                saved_report.write_bytes(browser_report.read_bytes())
                try:
                    cases.update(browser_cases(saved_report))
                except (ValueError, KeyError) as error:
                    stage.update(status="FAILED", reason=f"Invalid browser report: {error}")
    for number in range(1, 15):
        cases.setdefault(f"SEC{number}", {"status": "WARNING", "reason": "Browser case did not produce a result"})
    cases["SEC16"] = {"status": "WARNING", "reason": "Real AWS account QA is manual; SigV4/provider contract tests ran separately"}
    report["cases"] = cases
    failed = any(item["status"] == "FAILED" for item in report["stages"]) or any(item["status"] != "PASS" for key, item in cases.items() if int(key[3:]) <= 14)
    report["exitCode"] = int(failed)
    (results / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    lines = ["# PipeSecrets QA", "", f"Revision: `{report['revision']}`; dirty: `{report['dirty']}`.", "", "| Case | Result |", "| --- | --- |"]
    lines += [f"| {case} | {result['status']} |" for case, result in sorted(cases.items(), key=lambda item: int(item[0][3:]))]
    lines += ["", "SEC16 requires separate, scoped AWS account QA.", "", f"Exit code: {int(failed)}. Details: summary.json.", ""]
    (results / "summary.md").write_text("\n".join(lines))
    print(f"Report: {results / 'summary.json'}", flush=True)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
