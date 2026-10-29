"""Load disposable credentials safely and verify their production ownership.

These functions never create, delete, or publish an address. Registration sends
an empty JSON object, so it does not change the address's current target.
"""

from dataclasses import dataclass, field
import http.client
import json
import os
from pathlib import PurePosixPath
import re
import stat
import ssl


HOST = "tunnel.firecrab.dev"
MAX_RESPONSE = 8192


class CredentialError(Exception):
    """Credential validation failed without exposing credential contents."""


@dataclass(frozen=True)
class Credential:
    label: str
    path: str
    token: str = field(repr=False)


def _validate_label(label):
    if not isinstance(label, str) or not re.fullmatch(r"qa-[a-z0-9](?:[a-z0-9-]{0,56}[a-z0-9])?", label):
        raise CredentialError("test labels must start with qa- and use lowercase DNS characters")


def _read_token(path):
    # Walk directory FDs so a symlink cannot replace any path component between
    # inspection and opening the file. No credential bytes enter exception text.
    if not isinstance(path, (str, os.PathLike)):
        raise CredentialError("token path must be absolute")
    path = os.fspath(path)
    if not isinstance(path, str) or not path.startswith("/") or ".." in path.split("/"):
        raise CredentialError("token path must be absolute and contain no parent traversal")
    parts = PurePosixPath(path).parts[1:]
    if not parts or not hasattr(os, "O_NOFOLLOW"):
        raise CredentialError("safe token file access is unavailable")
    directory = None
    token_fd = None
    try:
        directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        token_fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        info = os.fstat(token_fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid not in (os.getuid(), os.geteuid())
                or stat.S_IMODE(info.st_mode) not in (0o400, 0o600)):
            raise CredentialError("token file must be a private, singly linked regular file owned by the current user")
        raw = os.read(token_fd, 67)
        if len(raw) == 65 and raw.endswith(b"\n"):
            raw = raw[:-1]
        if not re.fullmatch(rb"[a-f0-9]{64}", raw):
            raise CredentialError("token file must contain one 64-character lowercase hexadecimal token")
        return raw.decode("ascii"), (info.st_dev, info.st_ino)
    except OSError:
        raise CredentialError("cannot safely open the private token file") from None
    finally:
        if token_fd is not None:
            os.close(token_fd)
        if directory is not None:
            os.close(directory)


def load_credentials(entries):
    """Load exactly two distinct test address/token pairs from private files."""
    entries = list(entries)
    if len(entries) != 2:
        raise CredentialError("two disposable test credentials are required")
    result = []
    identities = set()
    for label, path in entries:
        _validate_label(label)
        token, identity = _read_token(path)
        result.append(Credential(label, os.fspath(path), token))
        identities.add(identity)
    if (len(identities) != 2 or result[0].label == result[1].label
            or result[0].token == result[1].token):
        raise CredentialError("test labels, token files, and tokens must be distinct")
    return tuple(result)


def _registration_response(credential):
    _validate_label(credential.label)
    if not re.fullmatch(r"[a-f0-9]{64}", credential.token):
        raise CredentialError("invalid credential")
    connection = None
    try:
        context = ssl.create_default_context()
        connection = http.client.HTTPSConnection(HOST, timeout=15, context=context)
        connection.request("POST", "/v1/register", body=b"{}", headers={
            "Authorization": "Bearer " + credential.token,
            "Content-Type": "application/json",
            "X-Firecrab-Protocol": "2",
        })
        response = connection.getresponse()
        body = response.read(MAX_RESPONSE + 1)
        if len(body) > MAX_RESPONSE:
            raise CredentialError("registration response is too large")
        return response.status, body
    except (OSError, http.client.HTTPException, ssl.SSLError):
        raise CredentialError("registration request failed") from None
    finally:
        if connection is not None:
            connection.close()


def verify_registration(credential):
    """Confirm that the token resolves to its exact disposable public address."""
    status, body = _registration_response(credential)
    if status != 200:
        raise CredentialError(f"registration rejected with HTTP {status}")
    try:
        response = json.loads(body)
    except (ValueError, UnicodeError):
        raise CredentialError("registration returned invalid JSON") from None
    url = f"https://{credential.label}.firecrab.dev"
    if (not isinstance(response, dict) or response.get("name") != credential.label
            or response.get("url") != url or response.get("heartbeat_url") != url + "/__firecrab/heartbeat"
            or response.get("suspended") is not False):
        raise CredentialError("registration identity, heartbeat URL, or suspension status did not match")
    return {"label": credential.label, "url": url, "status": status}


def verify_revoked(credential):
    """Confirm authentication rejects a token after operator revocation."""
    status, _ = _registration_response(credential)
    if status not in (401, 410):
        raise CredentialError(f"registration did not reject the revoked token (HTTP {status})")
    return {"label": credential.label, "status": status}
