#!/usr/bin/env python3
"""Build the API's current stable kernel before dependent guest QA starts."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import time
import urllib.request


def request(api, path, method="GET"):
    req = urllib.request.Request(api.rstrip("/") + path, method=method)
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.load(response)


def prepare(api, images, timeout):
    catalog = request(api, "/api/kernels")
    if not catalog or not re.fullmatch(r"[1-9][0-9]*\.[0-9]+(?:\.[0-9]+)?", catalog[0]["version"]):
        raise RuntimeError("kernel catalog did not return an upstream stable release")
    version = catalog[0]["version"]
    install = "/api/kernels/stable/install"
    request(api, install, "POST")
    deadline = time.monotonic() + timeout
    previous = ""
    while time.monotonic() < deadline:
        job = request(api, install)
        log = job.get("log", "")
        print(log[len(previous):] if log.startswith(previous) else log, end="", flush=True)
        previous = log
        if job["status"] == "succeeded":
            break
        if job["status"] != "running":
            raise RuntimeError(f"stable kernel installation {job['status']}: {log}")
        time.sleep(2)
    else:
        raise TimeoutError(f"stable kernel installation exceeded {timeout} seconds")

    entry = next((entry for entry in request(api, "/api/kernels") if entry["version"] == version), None)
    if not entry or not entry["installed"]:
        raise RuntimeError("completed stable kernel is not installed in the catalog")
    architecture = entry["architecture"]
    if architecture not in ("x86_64", "aarch64") or Path(entry["image"]).name != entry["image"]:
        raise RuntimeError("unexpected kernel architecture or filename")
    directory = images.resolve() / ".oci/kernel" / architecture
    kernel = directory / entry["image"]
    if not kernel.resolve().is_relative_to(directory):
        raise RuntimeError("kernel image escapes its architecture cache")
    receipt = json.loads(Path(str(kernel) + ".json").read_text())
    if receipt["version"] != version or receipt["architecture"] != architecture:
        raise RuntimeError("kernel receipt does not match the selected release")
    digest = hashlib.sha256()
    with kernel.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != entry["imageSha256"] or digest.hexdigest() != receipt["image_sha256"]:
        raise RuntimeError("kernel image digest does not match its catalog and receipt")
    return {
        "version": version,
        "architecture": architecture,
        "kernel": str(kernel),
        "expectedRelease": f"{version}-{receipt['build_revision']}",
        "imageSha256": digest.hexdigest(),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:5523")
    parser.add_argument("--images", type=Path, default=Path("/var/lib/firecrab/images"))
    parser.add_argument("--timeout", type=float, default=1800)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--github-env", type=Path)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    result = prepare(args.api, args.images, args.timeout)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    if args.github_env:
        values = {"FIRECRAB_CI_KERNEL_PATH": result["kernel"], "FIRECRAB_CI_KERNEL_RELEASE": result["expectedRelease"]}
        if any("\n" in value or "\r" in value for value in values.values()):
            raise RuntimeError("kernel environment values must be single-line")
        with args.github_env.open("a") as environment:
            for key, value in values.items():
                environment.write(f"{key}={value}\n")
    print(f"Verified stable kernel {result['expectedRelease']}: {result['kernel']}", flush=True)


if __name__ == "__main__":
    main()
