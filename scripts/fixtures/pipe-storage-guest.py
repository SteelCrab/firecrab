#!/usr/bin/env python3
"""Disposable MicroVM app: exercise boto3's normal environment credential provider."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import uuid

import boto3
from botocore.exceptions import ClientError


BOOT = str(uuid.uuid4())
KEY = os.environ["FIRECRAB_S3_PREFIX"] + "microvm-persistence.bin"
BUCKET = os.environ["FIRECRAB_S3_BUCKET"]
S3 = boto3.client("s3")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, status, value):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, {"ready": True, "boot": BOOT,
                "kernel": platform.release(), "hostname": platform.node(),
                "linuxBootId": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
                "sdk": "boto3", "sdkVersion": boto3.__version__})
        if self.path != "/read":
            return self.reply(404, {"error": "Unknown operation"})
        try:
            data = S3.get_object(Bucket=BUCKET, Key=KEY)["Body"].read()
            self.reply(200, {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "boot": BOOT})
        except ClientError as error:
            self.reply(502, {"error": error.response["Error"]["Code"]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        if self.path != "/write" or not 1 <= length <= 65536:
            return self.reply(400, {"error": "Use /write with 1-65536 bytes"})
        data = self.rfile.read(length)
        try:
            S3.put_object(Bucket=BUCKET, Key=KEY, Body=data)
            self.reply(200, {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "boot": BOOT})
        except ClientError as error:
            self.reply(502, {"error": error.response["Error"]["Code"]})


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
