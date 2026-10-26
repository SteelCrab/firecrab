"""Public guest-only QA payload. Execute only through an approved V1 task later."""
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import secrets
import sys
import time

LITERAL_ARGS = ["space argument", "single'quote", 'double"quote',
                "$HOME;$(printf no-shell);*?[]", "line\nbreak", ""]
LITERAL_ENV = "literal $HOME;$(printf no-shell)\nvalue"
STDIN = b"public-input\x00bytes\n"
CASES = {"literal", "memory", "wall", "output", "cancel-ready", "cancel-running"}


def evidence(case, run_id):
    status = dict(line.split(":", 1) for line in Path("/proc/self/status").read_text().splitlines()
                  if ":" in line)
    fields = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()
    return {
        "version": 1, "case": case, "runId": run_id, "nonce": secrets.token_hex(16),
        "pid": os.getpid(), "startTicks": int(fields[19]),
        "bootId": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
        "uid": os.getuid(), "euid": os.geteuid(), "gid": os.getgid(), "egid": os.getegid(),
        "groups": os.getgroups(), "noNewPrivs": int(status["NoNewPrivs"].strip()),
        "capEff": status["CapEff"].strip(), "cwdName": Path.cwd().name,
        "openrcRunPresent": Path("/sbin/openrc-run").is_file(),
        "openrcStatePresent": Path("/run/openrc").is_dir(),
        "pid1Comm": Path("/proc/1/comm").read_text().strip(),
        "addressSpaceLimit": list(resource.getrlimit(resource.RLIMIT_AS)),
        "cpuLimit": list(resource.getrlimit(resource.RLIMIT_CPU)),
        "nofileLimit": list(resource.getrlimit(resource.RLIMIT_NOFILE)),
    }


def main():
    case, run_id = sys.argv[1:3]
    if case not in CASES or not re.fullmatch(r"[0-9a-f]{32}", run_id):
        raise ValueError("Invalid public QA case/run identity")
    result = evidence(case, run_id)
    if (result["uid"] == 0 or result["euid"] == 0 or result["groups"]
            or result["noNewPrivs"] != 1 or int(result["capEff"], 16)
            or result["cwdName"] != "qa cwd"
            or not (result["openrcRunPresent"] or result["openrcStatePresent"])):
        raise RuntimeError("OpenRC guest privilege/cwd contract is unsupported or mismatched")
    if any(name in os.environ for name in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "SSH_AUTH_SOCK")):
        raise RuntimeError("Unexpected credential/agent environment presence")
    expected_memory = 64 if case == "memory" else 96
    if result["addressSpaceLimit"] != [expected_memory * 1024 * 1024] * 2 or result["cpuLimit"] != [3, 3]:
        raise RuntimeError("Actual guest hard limits differ from the admitted request")
    if result["nofileLimit"] != [1024, 1024]:
        raise RuntimeError("Actual guest nofile limit differs from the guest wrapper")
    if os.environ.get("QA_V1_RUN") != run_id or os.environ.get("QA_V1_LITERAL") != LITERAL_ENV:
        raise RuntimeError("Literal environment was changed")
    if case == "literal":
        stdin = sys.stdin.buffer.read(65537)
        if sys.argv[3:] != LITERAL_ARGS or stdin != STDIN:
            raise RuntimeError("Literal argv or NUL-containing stdin was changed")
        result.update(argv=sys.argv[3:], env={"QA_V1_LITERAL": LITERAL_ENV, "QA_V1_RUN": run_id},
                      stdinHex=stdin.hex(), stdinSha256=hashlib.sha256(stdin).hexdigest())
    elif sys.argv[3:] or sys.stdin.buffer.read(1):
        raise RuntimeError("Unexpected arguments or stdin")
    if case == "cancel-running":
        ready = Path(f"{run_id}-cancel-ready.json").read_bytes()
        prior = json.loads(ready)
        if prior["runId"] != run_id or prior["case"] != "cancel-ready" or not re.fullmatch(r"[0-9a-f]{32}", prior["nonce"]):
            raise RuntimeError("Cancel preparation evidence changed")
        result.update(nonce=prior["nonce"], readySha256=hashlib.sha256(ready).hexdigest())
    # Exclusive creation detects replay in retained Workspaces; never truncate an earlier receipt.
    with Path(f"{run_id}-{case}.json").open("x") as output:
        json.dump(result, output, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    print(f"OPENRCV1:{run_id}:{case}:stdout:{result['nonce']}", flush=True)
    print(f"OPENRCV1:{run_id}:{case}:stderr", file=sys.stderr, flush=True)
    if case == "memory":
        # Let the actual allocator/kernel fail. Do not catch it or invent a successful exit.
        bytearray(256 * 1024 * 1024)
        raise RuntimeError("Requested memory limit did not reject the oversized allocation")
    if case in ("wall", "cancel-running"):
        time.sleep(90)
        raise RuntimeError("Expected timeout/cancel did not arrive before the bounded sleep ended")
    if case == "output":
        sys.stdout.write("O" * 8192)
        sys.stdout.flush()
        sys.stderr.write("E" * 8192)
        sys.stderr.flush()
        raise RuntimeError("Output budget did not terminate the command")
    return 0 if case == "cancel-ready" else 7


if __name__ == "__main__":
    raise SystemExit(main())
