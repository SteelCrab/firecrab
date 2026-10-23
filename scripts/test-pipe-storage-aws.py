#!/usr/bin/env python3
"""Opt-in real AWS S3 QA against an isolated local Firecrab API.

Uses the local AWS CLI profile only to create and remove a tagged test bucket
and a bucket-scoped temporary IAM role. A disposable IAM user assumes that role;
the API receives only role session keys. The user's bootstrap key stays in memory.
Always cleans up in finally. After interruption, use --cleanup LEDGER before
removing the worktree. The ledger and report never contain credential values.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen
import uuid


class AwsError(RuntimeError):
    pass


def aws(service, operation, credentials=None, **arguments):
    environment = {**os.environ, "AWS_PAGER": ""}
    if credentials:
        environment.update(AWS_ACCESS_KEY_ID=credentials["AccessKeyId"],
                           AWS_SECRET_ACCESS_KEY=credentials["SecretAccessKey"],
                           AWS_SESSION_TOKEN=credentials.get("SessionToken", ""))
    result = subprocess.run(["aws", service, operation, "--cli-input-json", json.dumps(arguments), "--output", "json"],
                            env=environment, capture_output=True, text=True, timeout=45)
    if result.returncode:
        match = re.search(r"\(([A-Za-z0-9]+)\)", result.stderr)
        raise AwsError(f"{service}:{operation}: {match.group(1) if match else 'request failed'}")
    return json.loads(result.stdout) if result.stdout.strip() else {}


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with os.fdopen(os.open(temporary, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600), "w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
    temporary.replace(path)


def cleanup(path):
    record = json.loads(path.read_text())
    run = record["run"]
    expected = f"firecrab-439-{run}"
    if record.get("issue") != 439 or not re.fullmatch(r"[a-f0-9]{20}", run) or record["bucket"] != expected or record["role"] != expected or record.get("user", expected) != expected:
        raise ValueError("Refusing cleanup: invalid issue-439 resource ledger")
    identity = aws("sts", "get-caller-identity")
    if identity["Account"] != record["account"]:
        raise ValueError("Refusing cleanup from a different AWS account")
    errors = []
    try:
        page = aws("s3api", "list-objects-v2", Bucket=expected)
        while page.get("Contents"):
            if any(not item["Key"].startswith("qa/") for item in page["Contents"]):
                raise ValueError("Unexpected key outside the QA prefix; bucket retained")
            deleted = aws("s3api", "delete-objects", Bucket=expected,
                Delete={"Objects": [{"Key": item["Key"]} for item in page["Contents"]], "Quiet": True})
            if deleted.get("Errors"):
                raise AwsError("s3api:delete-objects: cleanup failed")
            page = aws("s3api", "list-objects-v2", Bucket=expected)
        aws("s3api", "delete-bucket", Bucket=expected)
    except AwsError as error:
        if not any(code in str(error) for code in ("NoSuchBucket", "NotFound")):
            errors.append(str(error))
    except ValueError as error:
        errors.append(str(error))
    for operation, arguments in [
        ("delete-role-policy", {"RoleName": expected, "PolicyName": "s3-qa"}),
        ("delete-role", {"RoleName": expected}),
    ]:
        try:
            aws("iam", operation, **arguments)
        except AwsError as error:
            if "NoSuchEntity" not in str(error):
                errors.append(str(error))
    if record.get("user"):
        try:
            keys = aws("iam", "list-access-keys", UserName=expected).get("AccessKeyMetadata", [])
            for key in keys:
                aws("iam", "delete-access-key", UserName=expected, AccessKeyId=key["AccessKeyId"])
        except AwsError as error:
            if "NoSuchEntity" not in str(error):
                errors.append(str(error))
        for operation, arguments in [
            ("delete-user-policy", {"UserName": expected, "PolicyName": "assume-s3-qa"}),
            ("delete-user", {"UserName": expected}),
        ]:
            try:
                aws("iam", operation, **arguments)
            except AwsError as error:
                if "NoSuchEntity" not in str(error):
                    errors.append(str(error))
    record["cleaned"] = not errors
    record["cleanupErrors"] = errors
    write_json(path, record)
    if errors:
        raise RuntimeError("AWS QA cleanup incomplete: " + "; ".join(errors))
    print("AWS test bucket, role, and disposable user/keys removed", flush=True)


def policy(bucket, read_only=False):
    return {"Version": "2012-10-17", "Statement": [
        {"Effect": "Allow", "Action": "s3:ListBucket", "Resource": f"arn:aws:s3:::{bucket}",
         "Condition": {"StringLike": {"s3:prefix": ["qa/", "qa/*"]}}},
        {"Effect": "Allow", "Action": ["s3:GetObject"] if read_only else ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
         "Resource": f"arn:aws:s3:::{bucket}/qa/*"},
    ]}


def wait_deployment(request, service, phase):
    deadline = time.monotonic() + 900
    while time.monotonic() < deadline:
        value = request("GET", f"/api/micro-services/{service}/deployment")
        if value and value["phase"] == phase:
            return value
        if value and value["phase"] == "failed":
            raise AssertionError("MicroVM deployment failed: " + (value.get("error") or "unknown"))
        time.sleep(2)
    raise AssertionError("MicroVM deployment timed out waiting for " + phase)


def microvm_checks(args, request, connection, services, report, checks):
    address = urlparse(args.microvm_url)
    if address.scheme != "http" or address.hostname not in ("127.0.0.1", "localhost"):
        raise ValueError("The QA MicroVM application must use a loopback/SSH-forwarded URL")
    evidence = report["microvm"] = {"passed": False}
    def app(path, body=None):
        with urlopen(Request(args.microvm_url + path, data=body), timeout=45) as response:
            return json.load(response)
    def ready(previous=None):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                value = app("/health")
                if value["ready"] and value["linuxBootId"] != previous:
                    assert value["sdk"] == "boto3"
                    return value
            except (OSError, ValueError):
                pass
            time.sleep(2)
        raise AssertionError("MicroVM application did not become ready")
    service = request("POST", "/api/micro-services", {"name": "s3-sdk-qa", "image": args.microvm_image,
        "port": 8080, "ram": 512, "cpu": 1, "startupTimeout": 180, "drainTimeout": 0,
        "storageConnectionId": connection}, expected=201)["id"]
    services.append(service)
    request("POST", f"/api/micro-services/{service}/deployment", {"hostPort": 28080}, expected=202)
    first = wait_deployment(request, service, "running")
    evidence["imageDigest"] = first["digest"]
    vm = first["instances"][0]["vmId"]
    evidence["firstVm"] = vm
    evidence["firstBoot"] = health = ready()
    payload = b"MicroVM boto3 external persistence\x00\xff\n" * 300
    expected = hashlib.sha256(payload).hexdigest()
    assert app("/write", payload)["sha256"] == expected
    assert app("/read")["sha256"] == expected
    assert request("GET", f"/api/pipe/storage/{connection}/object?" + urlencode({"key": "qa/microvm-persistence.bin"}), binary=True) == payload
    checks.append("real KVM MicroVM boto3 read/write through injected scoped credentials")
    print("MicroVM boto3 read/write passed", flush=True)
    request("POST", f"/api/vms/{vm}/stop")
    deadline = time.monotonic() + 120
    while request("GET", f"/api/vms/{vm}")["state"] != "stopped":
        if time.monotonic() > deadline:
            raise AssertionError("VM did not stop")
        time.sleep(1)
    request("POST", f"/api/vms/{vm}/start")
    evidence["restartedBoot"] = ready(health["linuxBootId"])
    assert app("/read")["sha256"] == expected
    checks.append("same MicroVM stop/start changes Linux boot ID and preserves S3 content")
    print("MicroVM restart persistence passed", flush=True)
    request("POST", f"/api/micro-services/{service}/stop", expected=202)
    assert not wait_deployment(request, service, "stopped")["instances"]
    request("POST", f"/api/micro-services/{service}/deployment", {"hostPort": 28080}, expected=202)
    replacement = wait_deployment(request, service, "running")
    evidence["replacementVm"] = replacement["instances"][0]["vmId"]
    assert evidence["replacementVm"] != vm
    evidence["replacementBoot"] = ready(evidence["restartedBoot"]["linuxBootId"])
    assert app("/read")["sha256"] == expected
    replacement_payload = payload + b"replacement"
    evidence["replacementSha256"] = hashlib.sha256(replacement_payload).hexdigest()
    assert app("/write", replacement_payload)["sha256"] == evidence["replacementSha256"]
    assert app("/read")["sha256"] == evidence["replacementSha256"]
    assert request("GET", f"/api/pipe/storage/{connection}/object?" + urlencode({"key": "qa/microvm-persistence.bin"}), binary=True) == replacement_payload
    evidence.update(passed=True, originalSha256=expected)
    checks.append("replacement MicroVM has a new VM/boot ID and reads/writes retained S3 data")
    print("MicroVM replacement persistence passed", flush=True)
    request("POST", f"/api/micro-services/{service}/stop", expected=202)
    wait_deployment(request, service, "stopped")
    request("DELETE", f"/api/micro-services/{service}", expected=204)
    services.remove(service)
    assert not {vm, evidence["replacementVm"]}.intersection(row["id"] for row in request("GET", "/api/vms"))
    evidence["instancesCleaned"] = True


def run(args):
    address = urlparse(args.api)
    if address.scheme != "http" or address.hostname not in ("127.0.0.1", "localhost"):
        raise ValueError("Use an isolated loopback API, not an installed or remote host")
    if not args.isolated_api:
        raise ValueError("Pass --isolated-api to confirm a disposable API/data directory")
    account = aws("sts", "get-caller-identity")["Account"]
    region = args.region or subprocess.check_output(["aws", "configure", "get", "region"], text=True).strip()
    run_id = uuid.uuid4().hex[:20]
    name = f"firecrab-439-{run_id}"
    ledger = args.output.with_name("s3-resources.json")
    if ledger.exists() and not json.loads(ledger.read_text()).get("cleaned"):
        raise ValueError(f"Clean up the previous run first: --cleanup {ledger}")
    report = {"issue": 439, "run": run_id, "account": account, "bucket": name, "role": name, "user": name, "region": region, "cleaned": False}
    write_json(ledger, report)
    connections, services = [], []
    checks = []

    def request(method, path, body=None, expected=200, binary=False):
        data = body if isinstance(body, bytes) else None if body is None else json.dumps(body).encode()
        headers = {"Content-Type": "application/octet-stream" if isinstance(body, bytes) else "application/json"}
        try:
            with urlopen(Request(args.api.rstrip("/") + path, data=data, headers=headers, method=method), timeout=125) as response:
                status, content = response.status, response.read()
        except HTTPError as error:
            status, content = error.code, error.read()
        if status != expected:
            raise AssertionError(f"{method} {path.split('?')[0]} returned {status}, expected {expected}")
        return content if binary else json.loads(content) if content else None

    def object_path(connection, key, overwrite=False):
        return f"/api/pipe/storage/{connection}/object?" + urlencode({"key": key, "overwrite": str(overwrite).lower()})

    def connect(credentials, label):
        result = request("POST", "/api/pipe/storage", {"name": label, "bucket": name, "region": region, "prefix": "qa/", "credentialRef": "connection",
            "credentials": {"accessKeyId": credentials["AccessKeyId"], "secretAccessKey": credentials["SecretAccessKey"], "sessionToken": credentials["SessionToken"]}}, expected=201)
        assert not any(secret in json.dumps(result) for secret in credentials.values() if isinstance(secret, str) and len(secret) > 20)
        connections.append(result["id"])
        return result["id"]

    try:
        print(f"Creating disposable AWS bucket and scoped role: {name}", flush=True)
        creation = {"Bucket": name}
        if region != "us-east-1":
            creation["CreateBucketConfiguration"] = {"LocationConstraint": region}
        aws("s3api", "create-bucket", **creation)
        aws("s3api", "put-bucket-tagging", Bucket=name, Tagging={"TagSet": [{"Key": "firecrab-qa", "Value": "439"}, {"Key": "run-id", "Value": run_id}]})
        user = aws("iam", "create-user", UserName=name, Tags=[{"Key": "firecrab-qa", "Value": "439"}, {"Key": "run-id", "Value": run_id}])["User"]
        trust = {"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": {"AWS": f"arn:aws:iam::{account}:root"}, "Action": "sts:AssumeRole", "Condition": {"ArnEquals": {"aws:PrincipalArn": user["Arn"]}}}]}
        role = aws("iam", "create-role", RoleName=name, AssumeRolePolicyDocument=json.dumps(trust), Tags=[{"Key": "firecrab-qa", "Value": "439"}, {"Key": "run-id", "Value": run_id}])["Role"]
        aws("iam", "put-role-policy", RoleName=name, PolicyName="s3-qa", PolicyDocument=json.dumps(policy(name)))
        aws("iam", "put-user-policy", UserName=name, PolicyName="assume-s3-qa", PolicyDocument=json.dumps({"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "sts:AssumeRole", "Resource": role["Arn"]}]}))
        bootstrap = aws("iam", "create-access-key", UserName=name)["AccessKey"]
        for attempt in range(12):
            try:
                full = aws("sts", "assume-role", credentials=bootstrap, RoleArn=role["Arn"], RoleSessionName="s3-qa", DurationSeconds=900)["Credentials"]
                break
            except AwsError:
                if attempt == 11:
                    raise
                time.sleep(3)
        for attempt in range(12):
            try:
                read_only = aws("sts", "assume-role", credentials=bootstrap, RoleArn=role["Arn"], RoleSessionName="s3-read-only", DurationSeconds=900, Policy=json.dumps(policy(name, True)))["Credentials"]
                break
            except AwsError:
                if attempt == 11:
                    raise
                time.sleep(3)
        del bootstrap
        for attempt in range(12):
            try:
                connection = connect(full, "AWS S3 QA")
                break
            except AssertionError:
                if attempt == 11:
                    raise
                time.sleep(3)
        assert request("POST", f"/api/pipe/storage/{connection}/verify") == {"list": "allowed", "read": "unknown", "write": "unknown", "delete": "unknown"}
        checks.append("scoped temporary credentials and non-mutating verification")
        try:
            aws("s3api", "list-buckets", credentials=full)
            raise AssertionError("Scoped role unexpectedly lists account buckets")
        except AwsError as error:
            assert "AccessDenied" in str(error)
        checks.append("no ListAllMyBuckets or ECR permission required")
        data = b"S3 integration binary bytes\x00\xff\n" * 100_000
        key = "qa/한글 +%?.bin"
        request("PUT", object_path(connection, key), data, expected=204)
        downloaded = request("GET", object_path(connection, key), binary=True)
        assert hashlib.sha256(downloaded).digest() == hashlib.sha256(data).digest()
        request("PUT", object_path(connection, "qa/second.bin"), b"second", expected=204)
        sdk_keys = [item["Key"] for item in aws("s3api", "list-objects-v2", credentials=full, Bucket=name, Prefix="qa/").get("Contents", [])]
        assert key in sdk_keys, f"The AWS SDK sees different object keys: {sdk_keys}"
        first = request("GET", f"/api/pipe/storage/{connection}/objects?limit=1")
        assert first["nextCursor"]
        second = request("GET", f"/api/pipe/storage/{connection}/objects?" + urlencode({"limit": 1, "cursor": first["nextCursor"]}))
        assert len(first["objects"]) == len(second["objects"]) == 1
        assert first["objects"][0]["key"] != second["objects"][0]["key"]
        request("PUT", object_path(connection, key), b"replacement", expected=409)
        request("PUT", object_path(connection, key, True), data, expected=204)
        request("GET", object_path(connection, "outside/key"), expected=400)
        checks.append("streamed 2 MiB+ roundtrip, Unicode keys, pagination, overwrite and prefix guards")
        if getattr(args, "microvm_image", None):
            microvm_checks(args, request, connection, services, report, checks)
        if args.cli:
            with tempfile.TemporaryDirectory(prefix="firecrab-439-cli-") as directory:
                source = Path(directory)/"source.bin"
                destination = Path(directory)/"download.bin"
                source.write_bytes(data)
                def cli(*arguments):
                    result = subprocess.run([str(args.cli), "--api", args.api, "pipe", "storage", *arguments], capture_output=True, timeout=130)
                    if result.returncode:
                        raise AssertionError("S3 CLI operation failed: " + arguments[0])
                cli("upload", connection, "qa/cli.bin", str(source))
                cli("download", connection, "qa/cli.bin", str(destination))
                assert hashlib.sha256(destination.read_bytes()).digest() == hashlib.sha256(data).digest()
                cli("objects", connection, "--limit", "1")
                cli("remove-object", connection, "qa/cli.bin", "--yes")
            checks.append("real CLI upload/download/list/delete and content hash")
        readonly_id = connect(read_only, "Read-only S3 QA")
        assert request("GET", object_path(readonly_id, key), binary=True) == data
        request("PUT", object_path(readonly_id, "qa/denied.bin"), b"denied", expected=403)
        request("DELETE", object_path(readonly_id, key), expected=403)
        if args.browser:
            environment = {**os.environ, "FIRECRAB_STORAGE_QA_URL": args.api,
                           "FIRECRAB_STORAGE_QA_SHA256": hashlib.sha256(data).hexdigest()}
            subprocess.run(["node", "firecrab-e2e/node_modules/@playwright/test/cli.js", "test", "--config", "firecrab-e2e/playwright.storage-live.config.ts"], env=environment, check=True, timeout=180)
            checks.append("real S3 dashboard listing/download and denied mutations on desktop/mobile")
        if getattr(args, "local_aws_auth", False):
            result = subprocess.run(["aws", "configure", "export-credentials", "--format", "process"], capture_output=True, text=True, check=True)
            credentials = json.loads(result.stdout)
            environment = {**os.environ, "FIRECRAB_STORAGE_QA_URL": args.api,
                "FIRECRAB_STORAGE_QA_BUCKET": name, "FIRECRAB_STORAGE_QA_REGION": region,
                "FIRECRAB_STORAGE_QA_ACCOUNT": account,
                "FIRECRAB_STORAGE_QA_ACCOUNT_CREDENTIALS": json.dumps(credentials)}
            try:
                subprocess.run(["node", "firecrab-e2e/node_modules/@playwright/test/cli.js", "test", "--config", "firecrab-e2e/playwright.storage-live.config.ts", "pipe-storage-account-live.spec.ts"], env=environment, check=True, timeout=180)
            finally:
                del credentials, environment, result
            checks.append("local AWS account authenticated through Auth; shared-reference S3 dashboard roundtrip/screenshots")
        checks.append("real read-only IAM session permits download and denies writes/deletes")
        invalid = {"name": "invalid replacement", "bucket": name, "region": region, "prefix": "qa/", "credentialRef": "connection",
                   "credentials": {"accessKeyId": full["AccessKeyId"], "secretAccessKey": "invalid-signing-secret", "sessionToken": full["SessionToken"]}}
        request("PUT", f"/api/pipe/storage/{connection}", invalid, expected=401)
        assert request("GET", f"/api/pipe/storage/{connection}")["name"] == "AWS S3 QA"
        checks.append("invalid credential replacement preserves working connection")
        service = request("POST", "/api/micro-services", {"name": "s3-qa-" + run_id[:8], "image": "example/app:v1", "port": 8080, "ram": 128, "cpu": 1, "storageConnectionId": connection}, expected=201)
        services.append(service["id"])
        request("DELETE", f"/api/pipe/storage/{connection}", expected=409)
        request("DELETE", f"/api/micro-services/{service['id']}", expected=204)
        services.remove(service["id"])
        request("DELETE", f"/api/pipe/storage/{connection}", expected=204)
        connections.remove(connection)
        request("GET", object_path(readonly_id, key), binary=True)
        checks.append("service binding blocks disconnect; disconnect preserves external data")
        request("DELETE", object_path(readonly_id, "qa/missing"), expected=403)
        request("DELETE", object_path(readonly_id, key), expected=403)
        report["checks"] = checks
        report["passed"] = True
    finally:
        api_cleanup_errors = []
        try:
            if getattr(args, "local_aws_auth", False):
                try:
                    connections.extend(row["id"] for row in request("GET", "/api/pipe/storage")
                        if row["name"] == "Local AWS account QA" and row["id"] not in connections)
                except Exception:
                    api_cleanup_errors.append("local-account-connection-discovery")
            if getattr(args, "microvm_image", None):
                for service in services:
                    try:
                        deployment = request("GET", f"/api/micro-services/{service}/deployment")
                        if deployment and deployment["instances"]:
                            request("POST", f"/api/micro-services/{service}/stop", expected=202)
                            wait_deployment(request, service, "stopped")
                    except Exception:
                        api_cleanup_errors.append("microvm-stop/" + service)
            for path in [*(f"/api/micro-services/{service}" for service in services), *(f"/api/pipe/storage/{connection}" for connection in connections)]:
                try:
                    request("DELETE", path, expected=204)
                except Exception:
                    api_cleanup_errors.append(path)
            if getattr(args, "local_aws_auth", False):
                try:
                    request("DELETE", "/api/pipe/auth/aws")
                except Exception:
                    api_cleanup_errors.append("pipe-auth-aws")
        finally:
            try:
                cleanup(ledger)
            finally:
                report["cleaned"] = json.loads(ledger.read_text())["cleaned"]
                report["checks"] = checks
                report["apiCleanupErrors"] = api_cleanup_errors
                write_json(args.output, report)
        if api_cleanup_errors:
            raise RuntimeError("Remove the isolated API data directory; some test connections could not be deleted")
    print(f"AWS S3 QA passed: {len(checks)} checks; report: {args.output}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", default="http://127.0.0.1:15539")
    parser.add_argument("--isolated-api", action="store_true")
    parser.add_argument("--region")
    parser.add_argument("--browser", action="store_true")
    parser.add_argument("--cli", type=Path)
    parser.add_argument("--microvm-image", help="Private QA image served in the isolated Linux network")
    parser.add_argument("--microvm-url", default="http://127.0.0.1:15540")
    parser.add_argument("--local-aws-auth", action="store_true", help="Authenticate the local AWS profile through the real dashboard (no traces)")
    parser.add_argument("--output", type=Path, default=Path("target/qa/s3-live.json"))
    parser.add_argument("--cleanup", type=Path)
    args = parser.parse_args()
    if args.cleanup:
        cleanup(args.cleanup)
    else:
        run(args)


if __name__ == "__main__":
    import signal
    def interrupted(signum, _frame):
        raise SystemExit(f"Interrupted by signal {signum}; cleaning test resources")
    signal.signal(signal.SIGTERM, interrupted)
    main()
