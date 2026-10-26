#!/usr/bin/env python3
"""Dependency-free CI client. Never print credentials or upstream response bodies."""
import ipaddress
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


class Failure(Exception):
    pass


class HttpFailure(Failure):
    def __init__(self, status):
        self.status = status
        super().__init__(f"Deploy endpoint returned HTTP {status}")


class Transient(Failure):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def identity(value):
    try:
        result = uuid.UUID(value)
        if result.int:
            return str(result)
    except (ValueError, TypeError, AttributeError):
        pass
    raise Failure("Deploy identities must be non-nil UUIDs")


def endpoint(value):
    try:
        url = urllib.parse.urlsplit(value)
        port = url.port
        loopback = url.hostname == "localhost"
        if not loopback:
            try:
                loopback = ipaddress.ip_address(url.hostname or "").is_loopback
            except ValueError:
                pass
        path = url.path.rstrip("/")
        local_path = re.fullmatch(r"/api/ci/services/([0-9a-f-]{36})/deployments", path)
        valid_path = path == "/__firecrab/deploy" or (loopback and local_path and identity(local_path[1]))
        if (url.scheme not in ("http", "https") or (url.scheme == "http" and not loopback)
                or not url.hostname or url.username is not None or url.password is not None
                or url.query or url.fragment or not valid_path or port == 0
                or any(ord(c) <= 32 or ord(c) >= 127 for c in value)):
            raise ValueError()
        return urllib.parse.urlunsplit((url.scheme, url.netloc, path, "", ""))
    except ValueError:
        raise Failure("Use an HTTPS service /__firecrab/deploy URL; HTTP is limited to loopback") from None


def image_reference(value):
    if not re.fullmatch(r"[a-z0-9][a-z0-9._:/-]*@sha256:[0-9a-f]{64}", value) or len(value) > 1024:
        raise Failure("Use a lowercase repository image pinned with @sha256")
    return value


class Client:
    def __init__(self, url, token, timeout=1800):
        self.url = endpoint(url)
        if not re.fullmatch(r"fcd_[0-9a-f]{32}_[A-Za-z0-9_-]{43}", token):
            raise Failure("A service Deploy token is required")
        if not 1 <= timeout <= 86400:
            raise Failure("Deploy timeout must be 1–86400 seconds")
        self.token = token
        self.deadline = time.monotonic() + timeout
        self.opener = urllib.request.build_opener(NoRedirect())

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise Failure("Deploy timed out; accepted work continues. Reuse the operation UUID to inspect it")
        return remaining

    def pause(self):
        time.sleep(min(2, self.remaining()))

    def request(self, method, suffix="", body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + suffix, data=data, method=method, headers={
            "Authorization": "Bearer " + self.token, "Content-Type": "application/json", "Accept": "application/json",
        })
        try:
            with self.opener.open(request, timeout=min(15, self.remaining())) as response:
                raw = response.read(65537)
                if len(raw) > 65536:
                    raise Failure("Deploy response exceeds the size limit")
                value = json.loads(raw)
                if not isinstance(value, dict):
                    raise ValueError()
                return value
        except urllib.error.HTTPError as error:
            raise HttpFailure(error.code) from None
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError):
            raise Transient("Deploy endpoint is temporarily unavailable") from None
        except (ValueError, UnicodeError):
            raise Failure("Deploy endpoint returned an invalid response") from None


def transient(error):
    return isinstance(error, Transient) or isinstance(error, HttpFailure) and error.status in (429, 502, 503, 504)


def status(value, operation, image, strategy, expected):
    if (identity(value.get("operationId")) != operation or value.get("image") != image
            or value.get("strategy") != strategy or (expected and value.get("expectedReleaseId") != expected)):
        raise Failure("Operation UUID belongs to different Deploy inputs")
    phases = {"queued", "importing", "starting", "checking_health", "connecting_domain", "switching_traffic",
              "draining", "rolling_back", "running", "degraded", "stopping", "stopped", "cancelled", "failed"}
    if (value.get("phase") not in phases or type(value.get("completed")) is not bool
            or type(value.get("succeeded")) is not bool):
        raise Failure("Deploy endpoint returned an invalid outcome")
    digest = value.get("digest")
    if digest is not None and not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise Failure("Deploy endpoint returned an invalid digest")
    return {"serviceId": identity(value.get("serviceId")), "operationId": operation,
            "expectedReleaseId": identity(value.get("expectedReleaseId")), "image": image, "strategy": strategy,
            "phase": value["phase"], "completed": value["completed"], "succeeded": value["succeeded"], "digest": digest}


def deploy(client, image, operation, expected=None, strategy="rolling"):
    image = image_reference(image)
    operation = identity(operation)
    expected = identity(expected) if expected else None
    if strategy not in ("rolling", "blue_green"):
        raise Failure("Deploy strategy must be rolling or blue_green")
    suffix = "/" + operation
    body = None
    last_phase = None
    while True:
        client.remaining()
        try:
            value = client.request("GET", suffix)
        except HttpFailure as error:
            if error.status != 404:
                if transient(error):
                    client.pause()
                    continue
                raise
            try:
                if body is None:
                    release = expected or identity(client.request("GET").get("releaseId"))
                    body = {"operationId": operation, "expectedReleaseId": release, "image": image, "strategy": strategy}
                value = client.request("POST", body=body)
            except Failure as error:
                if transient(error):
                    client.pause()
                    continue
                raise
        except Failure as error:
            if transient(error):
                client.pause()
                continue
            raise
        result = status(value, operation, image, strategy, expected)
        if result["phase"] != last_phase:
            print(f"Deploy {operation}: {result['phase']}", flush=True)
            last_phase = result["phase"]
        if result["completed"]:
            if not result["succeeded"] or not result["digest"]:
                raise Failure("Deploy failed or was cancelled; inspect its exact operation with the operator API")
            return result
        client.pause()


def output(name, value):
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
            stream.write(f"{name}={value}\n")


def main():
    try:
        url = endpoint(os.environ.get("CI_DEPLOY_URL", ""))
        supplied = os.environ.get("CI_DEPLOY_OPERATION", "")
        key = os.environ.get("CI_DEPLOY_RUN_KEY", "")
        operation = identity(supplied) if supplied else str(uuid.uuid5(uuid.NAMESPACE_URL, key + "/" + url) if key else uuid.uuid4())
        output("operation-id", operation)
        print("Deploy operation: " + operation, flush=True)
        try:
            timeout = int(os.environ.get("CI_DEPLOY_TIMEOUT", "1800"))
        except ValueError:
            raise Failure("Deploy timeout must be an integer") from None
        client = Client(url, os.environ.get("CI_DEPLOY_TOKEN", ""), timeout)
        result = deploy(client, os.environ.get("CI_DEPLOY_IMAGE", ""), operation,
                        os.environ.get("CI_DEPLOY_EXPECTED") or None, os.environ.get("CI_DEPLOY_STRATEGY", "rolling"))
        output("digest", result["digest"])
        return 0
    except Failure as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
