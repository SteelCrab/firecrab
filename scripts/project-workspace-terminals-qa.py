#!/usr/bin/env python3
"""Real project PTYs, mutually authenticated users and runtime recovery for #550."""
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import time
import uuid


class ViewerSocket:
    def __init__(self, qa, path):
        self.buffer = bytearray()
        self.socket = qa.contexts["viewer"].wrap_socket(socket.create_connection(("127.0.0.1", 19550), timeout=5), server_hostname="127.0.0.1")
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall((f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:19550\r\nOrigin: {qa.base}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
            f"Sec-WebSocket-Protocol: firecrab.terminal.v1\r\nSec-WebSocket-Key: {key}\r\n\r\n").encode())
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise EOFError("viewer handshake ended")
            self.buffer += chunk
            assert len(self.buffer) < 16384
        headers, remaining = self.buffer.split(b"\r\n\r\n", 1)
        self.buffer = bytearray(remaining)
        assert headers.startswith(b"HTTP/1.1 101 "), headers
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        assert accept in headers

    def exact(self, length):
        while len(self.buffer) < length:
            data = self.socket.recv(65536)
            if not data:
                raise EOFError("viewer connection ended")
            self.buffer += data
        value = bytes(self.buffer[:length])
        del self.buffer[:length]
        return value

    def receive(self):
        first, second = self.exact(2)
        assert first & 0x80 and not second & 0x80
        size = second & 0x7f
        if size == 126:
            size = struct.unpack(">H", self.exact(2))[0]
        elif size == 127:
            size = struct.unpack(">Q", self.exact(8))[0]
        assert size <= 65536
        data = self.exact(size)
        if first & 15 == 8:
            return None
        assert first & 15 == 1
        return json.loads(data)

    def close(self):
        self.socket.close()


class TerminalGate:
    def __init__(self, qa):
        self.qa = qa
        self.primary = str(uuid.uuid4())
        self.secondary = str(uuid.uuid4())
        self.client = str(uuid.uuid4())
        self.request = {"label": "QA550 persistent PTY", "workingDirectory": "", "rows": 24, "columns": 80}
        self.secondary_request = dict(self.request, label="QA550 revoked PTY")
        self.nonce = str(uuid.uuid4())
        self.control = None
        self.controller = "editor"

    def path(self, session=None, suffix=""):
        return self.qa.workspace + "/terminals/" + (session or self.primary) + suffix

    def policy(self, name, vm, write=True):
        rights = ["read", "write", "execute"] if write else ["read"]
        grants = [{"resource": {"kind": "project", "id": self.qa.project}, "permissions": rights},
            {"resource": {"kind": "vm", "id": vm}, "permissions": ["read", "execute"] if write else ["read"]}]
        self.qa.request("PUT", "/api/ai-sandbox/access/" + self.qa.identities[name], {"grants": grants})

    def claim(self, actor, session=None, take_over=False):
        current = self.qa.request("GET", self.path(session), actor=actor)
        result = self.qa.request("POST", self.path(session, "/control"), {"clientId": str(uuid.uuid4()),
            "expectedGeneration": current["controlGeneration"], "takeOver": take_over}, actor=actor)
        return {k: result[k] for k in ("clientId", "generation", "token")}

    def send(self, command, actor=None, control=None, session=None, status=None):
        return self.qa.request("POST", self.path(session, "/input"), {"type": "input",
            "control": control or self.control, "contentBase64": base64.b64encode(command.encode()).decode()},
            status=status, actor=actor or self.controller)

    def output(self, session=None):
        cursor, data = 0, bytearray()
        for _ in range(256):
            page = self.qa.request("GET", self.path(session, "/output?cursor=" + str(cursor)), actor="viewer")
            data += base64.b64decode(page["contentBase64"], validate=True)
            cursor = page["next"]
            if cursor >= page["end"]:
                return bytes(data)
        raise AssertionError("replay did not finish")

    def until(self, probe, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = probe()
            if result:
                return result
            time.sleep(0.2)
        raise AssertionError("owned PTY condition did not settle")

    def process(self, session=None):
        identifier = uuid.UUID(session or self.primary).hex
        raw = base64.b64decode(self.qa.ssh("base64 /workspace/shared/.firecrab-workspace/terminals/" + identifier + "/state"))
        assert len(raw) == 88 and raw[:4] == b"FCTS"
        return {"uid": struct.unpack_from(">I", raw, 8)[0], "supervisor": struct.unpack_from(">I", raw, 16)[0],
            "child": struct.unpack_from(">I", raw, 20)[0], "supervisorTicks": struct.unpack_from(">Q", raw, 24)[0],
            "childTicks": struct.unpack_from(">Q", raw, 32)[0], "bootId": str(uuid.UUID(bytes=raw[40:56]))}

    def started(self, vm):
        self.vm = vm
        for name in ("editor", "writer"):
            self.policy(name, vm["id"])
        self.policy("viewer", vm["id"], False)
        created = self.qa.request("PUT", self.path(), self.request, actor="editor")
        assert created["status"] == "running"
        self.original = self.process()
        first = self.claim("editor")
        self.qa.request("GET", self.path(), actor="viewer")
        self.qa.request("POST", self.path(suffix="/control"), {"clientId": str(uuid.uuid4()), "expectedGeneration": first["generation"]}, status=403, actor="viewer")
        self.send("printf UNAUTHORIZED\n", actor="viewer", control=first, status=403)
        self.control = self.claim("writer", take_over=True)
        self.controller = "writer"
        self.send("printf STALE_CONTROLLER\n", actor="editor", control=first, status=403)
        self.send("export QA550_SESSION='" + self.nonce + "'; printf 'READY:%s\\n' \"$QA550_SESSION\"; printf once >> qa550-terminal-once\n")
        self.until(lambda: ("READY:" + self.nonce).encode() in self.output())
        self.qa.request("POST", self.path(suffix="/input"), {"type": "resize", "control": self.control, "rows": 42, "columns": 111}, actor="writer")
        self.send("stty size\n")
        self.until(lambda: b"42 111" in self.output())
        other = self.qa.request("PUT", self.path(self.secondary), self.secondary_request, actor="writer")
        assert other["status"] == "running"
        secondary_control = self.claim("writer", self.secondary)
        self.send("printf 'SECOND:%s\\n' \"${QA550_SESSION-unset}\"\n", actor="writer", control=secondary_control, session=self.secondary)
        self.until(lambda: b"SECOND:unset" in self.output(self.secondary))
        second = self.process(self.secondary)
        assert second["uid"] != self.original["uid"] and second["child"] != self.original["child"]
        self.qa.request("PUT", self.path(), self.request, actor="editor")
        assert self.process() == self.original
        self.qa.passed("mutual-tls-users-share-real-ptys-with-fenced-input-and-resize", primary=self.primary,
            secondary=self.secondary, primaryProcess=self.original, secondaryProcess=second,
            identities=self.qa.identities)
        for name in ("editor", "writer", "viewer"):
            file = self.qa.request("GET", self.qa.file_url(), actor=name)
            assert base64.b64decode(file["contentBase64"]) == self.qa.file_bytes
        self.qa.request("PUT", self.qa.file_url(), {"operationId": str(uuid.uuid4()),
            "expectedVersion": self.qa.file_save["version"]["id"], "contentBase64": "eA=="}, status=403, actor="viewer")
        missing = "/api/ai-sandbox/projects/" + str(uuid.uuid4()) + "/workspace/terminals"
        self.qa.request("GET", missing, status=403, actor="viewer")
        self.qa.passed("real-tls-viewer-cannot-write-files-or-cross-project-boundaries")

    def restarted(self):
        old, actor = self.control, self.controller
        self.send("printf OLD_API_LEASE\n", actor=actor, control=old, status=403)
        self.qa.request("PUT", self.path(), self.request, actor="editor")
        assert self.process() == self.original
        self.control = self.claim("editor")
        self.controller = "editor"
        self.send("printf 'RESUME:%s\\n' \"$QA550_SESSION\"\n")
        self.until(lambda: ("RESUME:" + self.nonce).encode() in self.output())
        assert self.qa.ssh("cat /workspace/shared/qa550-terminal-once") == "once"
        self.qa.passed("pty-reattaches-after-api-restart-without-duplicate-shell-or-input", process=self.process())

    def revocations(self):
        viewer = ViewerSocket(self.qa, self.path(suffix="/socket?cursor=0"))
        try:
            message = viewer.receive()
            assert message is not None and message["type"] in ("state", "output")
            self.qa.request("DELETE", "/api/ai-sandbox/access/" + self.qa.identities["viewer"])
            closed = False
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    if viewer.receive() is None:
                        closed = True
                        break
                except EOFError:
                    closed = True
                    break
            assert closed, "revoked viewer retained its WebSocket"
        finally:
            viewer.close()
        self.qa.request("GET", self.path(), status=403, actor="viewer")
        self.policy("viewer", self.vm["id"], False)
        self.policy("writer", self.vm["id"], False)
        self.qa.request("POST", self.path(suffix="/control"), {"clientId": str(uuid.uuid4()), "expectedGeneration": 0}, status=403, actor="writer")
        self.until(lambda: self.qa.request("GET", self.path(self.secondary), actor="viewer")["status"] in ("exited", "interrupted"))
        assert self.qa.request("GET", self.path(), actor="viewer")["status"] == "running"
        assert self.process() == self.original
        self.qa.passed("live-viewer-revocation-closes-socket-and-creator-revocation-stops-only-owned-pty")

    def stopped(self):
        assert self.qa.request("GET", self.path(), actor="viewer")["status"] == "interrupted"
        assert ("RESUME:" + self.nonce).encode() in self.output()
        self.qa.passed("terminal-history-queryable-through-real-tls-with-stopped-vm")

    def rebooted(self):
        restored = self.qa.request("PUT", self.path(), self.request, actor="editor")
        assert restored["status"] == "interrupted"
        assert self.qa.ssh("cat /workspace/shared/qa550-terminal-once") == "once"
        assert ("RESUME:" + self.nonce).encode() in self.output()
        self.qa.passed("vm-reboot-interrupts-pty-and-retains-history-without-replaying-commands")

    def replaced(self, vm):
        self.policy("editor", vm["id"])
        self.policy("viewer", vm["id"], False)
        self.rebooted()
        self.qa.passed("replacement-vm-retains-old-pty-history-and-never-relaunches-its-shell")
