#!/usr/bin/env python3
"""Fetch a fixed Alpine image layer for disposable service-security QA."""
import hashlib
import json
from pathlib import Path
import platform
import sys
import urllib.parse
import urllib.request

LAYERS = {
    "amd64": "fbcfea79c1c4819e0689385057cfd4cbd2ee9ba20e6d420b360644788a22862c",
    "arm64": "61254a502902280aa7caa832f8b3ed5c96aa04faf42ab0ba23ff17638f910f21",
}


class SafeRedirect(urllib.request.HTTPRedirectHandler):
    """Never forward a registry bearer token to a redirected blob host."""
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected and urllib.parse.urlsplit(req.full_url).netloc != urllib.parse.urlsplit(newurl).netloc:
            redirected.remove_header("Authorization")
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError("insecure blob redirect")
        return redirected


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: ci-qa-security-layer.py OUTPUT")
    arch = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(platform.machine())
    if arch not in LAYERS:
        raise SystemExit("unsupported security fixture architecture")
    expected = LAYERS[arch]
    opener = urllib.request.build_opener(SafeRedirect())
    with opener.open("https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/alpine:pull", timeout=30) as response:
        token = json.load(response)["token"]
    request = urllib.request.Request("https://registry-1.docker.io/v2/library/alpine/blobs/sha256:" + expected,
                                     headers={"Authorization": "Bearer " + token})
    with opener.open(request, timeout=120) as response:
        data = response.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != expected:
        raise SystemExit("Alpine layer failed digest or size verification")
    output = Path(sys.argv[1])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(data)
    print("Verified Alpine security fixture layer for", arch)


if __name__ == "__main__":
    main()
