#!/usr/bin/env python3
"""Check public Firecrab Tunnel routing without tokens or state changes.

This is a preflight only. It does not create an address, connect an app, or
prove that public traffic reaches a VM.
"""

import argparse
import contextlib
import http.client
import json
import socket
import ssl
import sys
import uuid


DOMAIN = "firecrab.dev"
CONSOLE = "console." + DOMAIN
REGISTRATION = "tunnel." + DOMAIN
MAX_RESPONSE = 8192


class CheckError(Exception):
    """A public preflight did not establish the expected behavior."""


def check_dns(host):
    try:
        addresses = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
    except OSError as error:
        raise CheckError(f"DNS lookup failed for {host}") from error
    if not addresses:
        raise CheckError(f"DNS returned no addresses for {host}")


def check_https(host, path, expected_status, *, headers=None, json_error=False):
    # Default TLS verification checks both the certificate chain and hostname.
    try:
        with contextlib.closing(http.client.HTTPSConnection(
            host, timeout=10, context=ssl.create_default_context()
        )) as connection:
            connection.request("GET", path, headers=headers or {})
            response = connection.getresponse()
            status = response.status
            content_type = response.getheader("Content-Type", "").split(";", 1)[0].lower()
            body = response.read(MAX_RESPONSE + 1) if json_error else b""
    except (OSError, ssl.SSLError, http.client.HTTPException) as error:
        raise CheckError(f"HTTPS request failed for {host}") from error
    if status != expected_status:
        raise CheckError(f"{host}{path} returned HTTP {status}, expected {expected_status}")
    if len(body) > MAX_RESPONSE:
        raise CheckError(f"{host}{path} returned an oversized response")
    if json_error:
        try:
            payload = json.loads(body)
        except (ValueError, UnicodeError) as error:
            raise CheckError(f"{host}{path} did not return JSON") from error
        if content_type != "application/json" or not isinstance(payload, dict) or not isinstance(payload.get("error"), str):
            raise CheckError(f"{host}{path} did not return the expected error response")
    return status


def preflight():
    # A fresh unregistered name tests wildcard DNS/TLS and the missing-address
    # response without disclosing or modifying another user's reservation.
    unknown = "qa-" + uuid.uuid4().hex + "." + DOMAIN
    for host in (CONSOLE, REGISTRATION, unknown):
        check_dns(host)
    check_https(CONSOLE, "/", 200)
    check_https(REGISTRATION, "/v1/register", 405,
                headers={"X-Firecrab-Protocol": "2"}, json_error=True)
    check_https(unknown, "/", 404, json_error=True)
    return {
        "consoleHttps": "passed",
        "registrationHttps": "passed",
        "wildcardDnsAndHttps": "passed",
        "publicApplicationAcceptance": "not_run",
    }


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    try:
        result = preflight()
    except CheckError as error:
        print(f"Public Tunnel preflight failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
