#!/usr/bin/env python3
"""Small pinned-image QA service: SQLite JSON/TCP 5432 and HTTP backend 8080.

This deliberately uses a bounded fixture protocol, not PostgreSQL. Auto-role
selection is only for a fresh dedicated /24 with Debug/Test/Backend/DB leases
in that order (.2/.3/.4/.5). Production discovery never relies on these offsets.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
from pathlib import Path
import socket
import socketserver
import sqlite3

SEED = "firecrab-environment-seed-414-v1"
LIMIT = 8192


def initialize(path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as database:
        database.execute("CREATE TABLE IF NOT EXISTS seed(identity TEXT PRIMARY KEY)")
        database.execute("INSERT OR IGNORE INTO seed(identity) VALUES(?)", (SEED,))
        database.execute("CREATE TABLE IF NOT EXISTS writes(key TEXT PRIMARY KEY,value TEXT NOT NULL)")


def database_request(path, request):
    if not isinstance(request, dict):
        raise ValueError("request must be an object")
    operation = request.get("operation")
    if operation not in ("read", "write"):
        raise ValueError("unsupported operation")
    with sqlite3.connect(path, timeout=3) as database:
        if operation == "write":
            key, value = request.get("key"), request.get("value")
            if not isinstance(key, str) or not 1 <= len(key) <= 64 or not all(char.isalnum() or char in "_.-" for char in key):
                raise ValueError("invalid key")
            if not isinstance(value, str) or len(value.encode()) > 4096:
                raise ValueError("invalid value")
            database.execute("INSERT INTO writes(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", (key, value))
        return {"seed": database.execute("SELECT identity FROM seed ORDER BY identity").fetchall(),
                "values": dict(database.execute("SELECT key,value FROM writes ORDER BY key"))}


class DatabaseHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(3)
        try:
            payload = self.rfile.readline(LIMIT + 1)
            if len(payload) > LIMIT or not payload.endswith(b"\n"):
                raise ValueError("request limit exceeded")
            reply = database_request(self.server.database_path, json.loads(payload))
            self.wfile.write(json.dumps(reply, separators=(",", ":")).encode() + b"\n")
        except (ValueError, sqlite3.Error, TimeoutError):
            self.wfile.write(b'{"error":"fixture_request_failed"}\n')


class DatabaseServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, address, path):
        initialize(path)
        self.database_path = path
        super().__init__(address, DatabaseHandler)


def remote_database(host, port, request):
    payload = json.dumps(request, separators=(",", ":")).encode() + b"\n"
    if len(payload) > LIMIT:
        raise ValueError("request limit exceeded")
    with socket.create_connection((host, port), timeout=3) as connection:
        connection.sendall(payload)
        with connection.makefile("rb") as stream:
            response = stream.readline(LIMIT + 1)
    if len(response) > LIMIT or not response.endswith(b"\n"):
        raise ValueError("response limit exceeded")
    return json.loads(response)


class BackendHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, operation):
        try:
            body = remote_database(self.server.database_host, self.server.database_port, operation)
            status = 200 if "error" not in body else 503
        except (OSError, ValueError):
            status, body = 503, {"error": "fixture_database_unavailable"}
        payload = json.dumps(body, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path not in ("/health", "/seed"):
            self.send_error(404)
            return
        self.reply({"operation": "read"})

    def do_POST(self):
        if self.path != "/write":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 1 <= length <= LIMIT:
                raise ValueError("request limit exceeded")
            body = json.loads(self.rfile.read(length))
            self.reply({"operation": "write", "key": body["key"], "value": body["value"]})
        except (ValueError, KeyError, TypeError):
            self.send_error(400)


class BackendServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, database_host, database_port):
        self.database_host, self.database_port = database_host, database_port
        super().__init__(address, BackendHandler)


def auto_role():
    # UDP connect only selects a local route. It does not send an Internet packet.
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
        route.connect(("192.0.2.1", 9))
        address = ipaddress.IPv4Address(route.getsockname()[0])
    role = {2: "idle", 3: "idle", 4: "backend", 5: "db"}.get(address.packed[-1])
    if role is None:
        raise ValueError("auto fixture requires fresh /24 leases .2/.3/.4/.5")
    database_host = str(ipaddress.IPv4Address((int(address) & 0xffffff00) + 5))
    return role, database_host


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--role", choices=("db", "backend", "auto"), default="auto")
    parser.add_argument("--listen", default="0.0.0.0")
    parser.add_argument("--database", default="/var/lib/firecrab-environment-seed/state.sqlite")
    parser.add_argument("--database-host")
    parser.add_argument("--database-port", type=int, default=5432)
    parser.add_argument("--http-port", type=int, default=8080)
    parser.add_argument("--init-only", action="store_true")
    args = parser.parse_args()
    if args.init_only:
        initialize(args.database)
        return
    role, database_host = auto_role() if args.role == "auto" else (args.role, args.database_host)
    if role == "idle":
        return
    if role == "db":
        server = DatabaseServer((args.listen, args.database_port), args.database)
    else:
        if not database_host:
            parser.error("backend requires --database-host")
        server = BackendServer((args.listen, args.http_port), database_host, args.database_port)
    with server:
        server.serve_forever()


if __name__ == "__main__":
    main()
