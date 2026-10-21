#!/usr/bin/env python3
"""Real HTTP API -> disposable loopback PostgreSQL/MySQL database QA.

Creates only randomly named fixture tables. Never connects to the installed API.
Database URIs come from environment variables, not command-line arguments.
"""

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote, unquote, urlsplit, urlunsplit
import uuid


ROOT = Path(__file__).resolve().parent.parent
ORIGIN = "http://localhost:8096"


def identifier(engine, name):
    delimiter = '"' if engine == "postgres" else "`"
    return delimiter + name.replace(delimiter, delimiter * 2) + delimiter


def source_metadata():
    revision = os.environ.get("FIRECRAB_QA_SOURCE_REVISION") or None
    dirty = None
    if git := shutil.which("git"):
        if not revision:
            revision = subprocess.run([git, "rev-parse", "HEAD"], cwd=ROOT, text=True, capture_output=True).stdout.strip() or None
        result = subprocess.run([git, "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True)
        if result.returncode == 0:
            dirty = bool(result.stdout)
    if "FIRECRAB_QA_SOURCE_DIRTY" in os.environ:
        dirty = os.environ["FIRECRAB_QA_SOURCE_DIRTY"] == "1"
    return revision, dirty


class Fixture:
    def __init__(self, engine, client):
        self.engine = engine
        self.client = shutil.which(client)
        if not self.client:
            raise ValueError(f"{engine} client is required")
        prefix = f"FIRECRAB_TEST_{engine.upper()}"
        self.uri = os.environ.get(prefix + "_URI", "")
        self.url = urlsplit(self.uri)
        schemes = {"postgres", "postgresql"} if engine == "postgres" else {"mysql"}
        if self.url.scheme not in schemes or self.url.hostname not in {"localhost", "127.0.0.1", "::1"}:
            raise ValueError(f"{prefix}_URI must refer to a disposable loopback database")
        self.user = unquote(self.url.username or "")
        self.password = unquote(self.url.password or "")
        self.schema = "public" if engine == "postgres" else unquote(self.url.path[1:])
        if not self.user or not self.url.path[1:] or not self.password:
            raise ValueError(f"{prefix}_URI requires a username, password and database")
        self.ca = Path(os.environ[prefix + "_CA"]).read_text() if os.environ.get(prefix + "_CA") else None
        self.ca_path = os.environ.get(prefix + "_CA")
        self.prefix = "qa_pipe_" + uuid.uuid4().hex[:12]
        self.names = {name: self.prefix + "_" + name for name in ["rows", "empty", "wide", 'odd"`name']}
        self.version = ""

    def table(self, name):
        return f"{identifier(self.engine, self.schema)}.{identifier(self.engine, self.names[name])}"

    def sql(self, text):
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("PG", "MYSQL_"))}
        port = self.url.port or (5432 if self.engine == "postgres" else 3306)
        database = unquote(self.url.path[1:])
        if self.engine == "postgres":
            environment.update(PGHOST=self.url.hostname, PGPORT=str(port), PGUSER=self.user,
                               PGPASSWORD=self.password, PGDATABASE=database, PGPASSFILE=os.devnull,
                               PGCONNECT_TIMEOUT="5", PGSSLMODE="verify-full" if self.ca_path else "disable")
            if self.ca_path:
                environment["PGSSLROOTCERT"] = self.ca_path
            command = [self.client, "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1"]
        else:
            environment.update(MYSQL_PWD=self.password, MYSQL_TEST_LOGIN_FILE=os.devnull)
            command = [self.client, "--no-defaults", "--protocol=TCP", "--connect-timeout=5",
                       "--host=" + self.url.hostname, "--port=" + str(port), "--user=" + self.user,
                       "--database=" + database, "--batch", "--skip-column-names", "--default-character-set=utf8mb4"]
            if self.ca_path:
                command.append("--ssl-ca=" + self.ca_path)
        result = subprocess.run(command, input=text, text=True, capture_output=True, env=environment, timeout=30)
        if result.returncode:
            # Client diagnostics can include credentials. Keep raw stderr private.
            raise RuntimeError(f"{self.engine} fixture SQL failed (exit {result.returncode})")
        return result.stdout.strip()

    def setup(self):
        self.version = self.sql("SELECT VERSION();")
        binary_type = "bytea" if self.engine == "postgres" else "blob"
        binary = "decode('0102', 'hex')" if self.engine == "postgres" else "X'0102'"
        columns = ", ".join(f"c{i} integer" for i in range(129))
        self.sql(f"""
            CREATE TABLE {self.table('rows')} (id integer, label text, note text, payload {binary_type}, long_value text);
            INSERT INTO {self.table('rows')} VALUES
              (1, '한글', NULL, {binary}, REPEAT('x', 3000)),
              (2, '', 'NULL', NULL, ''), (3, '<script>text</script>', 'plain', NULL, 'tail');
            CREATE TABLE {self.table('empty')} (id integer);
            CREATE TABLE {self.table('wide')} ({columns});
            CREATE TABLE {self.table('odd"`name')} (id integer);
            INSERT INTO {self.table('odd"`name')} VALUES (7);
        """)

    def fingerprint(self):
        binary = "encode(payload, 'hex')" if self.engine == "postgres" else "HEX(payload)"
        value = self.sql(f"SELECT id, label, note, {binary}, long_value FROM {self.table('rows')} ORDER BY id;")
        return hashlib.sha256(value.encode()).hexdigest()

    def cleanup(self):
        self.sql(";".join(f"DROP TABLE IF EXISTS {self.table(name)}" for name in self.names) + ";")

    def input(self, **extra):
        return {"name": "QA " + self.engine, "provider": "local", "uri": self.uri, "tlsMode": "disable", **extra}

    def wrong_password_uri(self):
        host = self.url.hostname
        if ":" in host:
            host = "[" + host + "]"
        netloc = f"{quote(self.user, safe='')}:qa-invalid-{uuid.uuid4().hex}@{host}:{self.url.port or (5432 if self.engine == 'postgres' else 3306)}"
        return urlunsplit(self.url._replace(netloc=netloc))


class Api:
    def __init__(self, binary, root, frontend=None):
        self.binary, self.root, self.frontend = binary, root, frontend
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.process = None
        self.log = (root / "api.log").open("w+")

    def start(self):
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("FIRECRAB_", "PG", "MYSQL_"))}
        environment.update(FIRECRAB_BIND_ADDR=f"127.0.0.1:{self.port}", FIRECRAB_ALLOWED_ORIGINS=f"{ORIGIN},http://127.0.0.1:{self.port}",
                           FIRECRAB_IMAGE_ROOT=str(self.root / "images"), FIRECRAB_BOOTSTRAP_WARMUP="0",
                           FIRECRAB_NET_HELPER_SOCK=str(self.root / "absent-helper.sock"), HOME=str(self.root))
        if self.frontend:
            environment["FIRECRAB_STATIC_ROOT"] = str(self.frontend)
        self.process = subprocess.Popen([str(self.binary)], cwd=self.root, env=environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("isolated API exited during startup")
            try:
                self.request("GET", "/api/pipe/databases", expected=200)
                if self.process.poll() is None:
                    return
            except (OSError, http.client.HTTPException):
                pass
            time.sleep(0.1)
        raise RuntimeError("isolated API did not become ready")

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)

    def request(self, method, path, body=None, expected=200, origin=ORIGIN):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=12)
        try:
            connection.request(method, path, body=None if body is None else json.dumps(body),
                               headers={"Content-Type": "application/json", "Origin": origin})
            response = connection.getresponse()
            payload = response.read()
            if response.status != expected:
                raise AssertionError(f"{method} {path}: expected HTTP {expected}, got {response.status}")
            return json.loads(payload)
        finally:
            connection.close()


def exercise(api, fixture, record, browser=False):
    base = "/api/pipe/databases"
    before = fixture.fingerprint()
    api.request("POST", base + "/test", fixture.input())
    assert api.request("GET", base) == [], "connection test persisted credentials"
    summary = api.request("POST", base, fixture.input(), expected=201)
    connection = base + "/" + summary["id"]
    assert summary["engine"] == fixture.engine and "uri" not in summary and "caCertificate" not in summary
    assert fixture.password not in json.dumps(summary), "save response exposed a password"
    record("connect", "PASS")

    tables = api.request("GET", connection + "/tables")["tables"]
    assert all({"schema": fixture.schema, "name": name} in tables for name in fixture.names.values())
    preview_request = {"schema": fixture.schema, "table": fixture.names["rows"], "limit": 50, "offset": 0}
    preview = api.request("POST", connection + "/preview", preview_request)
    rows = {row[0]: row for row in preview["rows"]}
    assert rows["1"][1] == "한글" and rows["1"][2] is None
    assert rows["2"][1] == "" and rows["2"][2] == "NULL"
    assert rows["1"][3] == ("\\x0102" if fixture.engine == "postgres" else "0102")
    assert len(rows["1"][4]) == 2048 and len(rows) == 3 and not preview["hasMore"]
    for offset in [0, 1, 2]:
        page = api.request("POST", connection + "/preview", {**preview_request, "limit": 1, "offset": offset})
        assert len(page["rows"]) == 1 and page["offset"] == offset and page["hasMore"] == (offset < 2)
    assert api.request("POST", connection + "/preview", {**preview_request, "table": fixture.names["empty"]})["rows"] == []
    assert api.request("POST", connection + "/preview", {**preview_request, "table": fixture.names['odd"`name']})["rows"] == [["7"]]
    record("tables-values-pagination", "PASS")

    for fields in [{"limit": 0}, {"limit": 101}, {"offset": 100001}, {"sql": "DROP TABLE users"}, {"table": fixture.names["wide"]}]:
        api.request("POST", connection + "/preview", {**preview_request, **fields}, expected=400)
    api.request("POST", connection + "/preview", {**preview_request, "table": 'missing"; DROP TABLE users; --'}, expected=404)
    api.request("POST", base + "/test", fixture.input(provider="rds"), expected=400)
    api.request("POST", base + "/test", fixture.input(uri="https://project.supabase.co"), expected=400)
    api.request("POST", base + "/test", fixture.input(uri=fixture.uri + "?sslrootcert=/etc/shadow"), expected=400)
    api.request("POST", base + "/test", fixture.input(), expected=403, origin="https://untrusted.example")
    failure = api.request("POST", base, fixture.input(uri=fixture.wrong_password_uri()), expected=502)
    assert fixture.password not in json.dumps(failure), "error response exposed a password"
    assert len(api.request("GET", base)) == 1, "failed connection was persisted"
    record("validation-authentication-origin", "PASS")

    if fixture.ca:
        providers = ["supabase", "rds"] if fixture.engine == "postgres" else ["rds"]
        for provider in providers:
            api.request("POST", base + "/test", fixture.input(provider=provider, tlsMode="verify_full", caCertificate=fixture.ca))
        api.request("POST", base + "/test", fixture.input(tlsMode="verify_full"), expected=502)
        record("tls-ca-verification", "PASS")
    else:
        record("tls-ca-verification", "WARNING", "No fixture CA supplied; TLS certificate checks were not run")

    mode = stat.S_IMODE((api.root / "data/firecrab.db").stat().st_mode)
    assert mode == 0o600, "saved credentials database is not owner-only"
    api.stop()
    api.start()
    listed = api.request("GET", base)
    assert len(listed) == 1 and listed[0]["id"] == summary["id"] and "uri" not in listed[0]
    assert fixture.password not in json.dumps(listed), "list response exposed a password"
    api.request("POST", connection + "/preview", preview_request)
    record("restart-persistence-permissions", "PASS")

    if browser:
        environment = dict(os.environ, FIRECRAB_PIPE_LIVE_URL=f"http://127.0.0.1:{api.port}",
                           FIRECRAB_PIPE_LIVE_ENGINE=fixture.engine,
                           FIRECRAB_PIPE_LIVE_DATABASE=json.dumps({"uri": fixture.uri, "schema": fixture.schema, "table": fixture.names["rows"]}))
        result = subprocess.run(["npm", "run", "test:pipe:live", "--prefix", str(ROOT / "firecrab-e2e")],
                                env=environment, timeout=180)
        assert result.returncode == 0, "real browser workflow failed"
        record("browser-to-api-to-database", "PASS")

    api.request("DELETE", connection)
    api.request("DELETE", connection, expected=404)
    api.request("GET", connection + "/tables", expected=404)
    assert api.request("GET", base) == []
    assert fixture.fingerprint() == before, "read-only operations changed the source data"
    record("delete-source-preservation", "PASS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True, type=Path)
    parser.add_argument("--psql", default="psql")
    parser.add_argument("--mysql", default="mysql")
    parser.add_argument("--output", type=Path, default=Path("logs/pipe-databases-qa.json"))
    parser.add_argument("--require-tls", action="store_true", help="fail if either fixture CA is missing")
    parser.add_argument("--frontend-dist", type=Path, help="built dashboard directory for real browser checks")
    parser.add_argument("--browser", action="store_true", help="run real desktop/mobile Playwright checks")
    args = parser.parse_args()
    os.umask(0o077)
    if platform.system() != "Linux" or not args.api.resolve().is_file():
        parser.error("requires Linux and an existing firecrab-api binary")
    if args.browser and (not args.frontend_dist or not (args.frontend_dist / "index.html").is_file()):
        parser.error("--browser requires --frontend-dist with a built dashboard")
    try:
        fixtures = [Fixture("postgres", args.psql), Fixture("mysql", args.mysql)]
        if args.require_tls and any(not fixture.ca for fixture in fixtures):
            raise ValueError("--require-tls requires FIRECRAB_TEST_POSTGRES_CA and FIRECRAB_TEST_MYSQL_CA")
    except (ValueError, OSError) as error:
        parser.error(str(error))
    results = []
    failed = False
    logs = []
    for fixture in fixtures:
        def record(name, status, detail=""):
            results.append({"engine": fixture.engine, "case": name, "status": status, "detail": detail})
            print(f"{status} {fixture.engine} {name}", flush=True)
        with tempfile.TemporaryDirectory(prefix="fc-pipe-database-api-") as directory:
            api = Api(args.api.resolve(), Path(directory), args.frontend_dist.resolve() if args.frontend_dist else None)
            try:
                fixture.setup()
                api.start()
                exercise(api, fixture, record, args.browser)
            except (AssertionError, OSError, RuntimeError, subprocess.SubprocessError, ValueError, KeyError) as error:
                failed = True
                record("execution", "FAILED", str(error))
            finally:
                api.stop()
                api.log.seek(0)
                log = api.log.read()
                if fixture.password in log or fixture.uri in log:
                    failed = True
                    record("credential-log-redaction", "FAILED", "API log contains connection credentials")
                else:
                    record("credential-log-redaction", "PASS")
                logs.append(log)
                api.log.close()
                try:
                    fixture.cleanup()
                    record("fixture-cleanup", "PASS")
                except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                    failed = True
                    record("fixture-cleanup", "WARNING", str(error))
    secrets = [value for fixture in fixtures for value in [fixture.uri, fixture.password] if value]
    def redact(value):
        for secret in sorted(secrets, key=len, reverse=True):
            value = value.replace(secret, "[REDACTED]")
        return value
    revision, dirty = source_metadata()
    report = {"sourceRevision": revision, "dirty": dirty, "platform": platform.platform(), "architecture": platform.machine(),
              "command": sys.argv, "exitCode": int(failed), "databaseVersions": {fixture.engine: fixture.version for fixture in fixtures}, "results": results}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(redact(json.dumps(report, indent=2, ensure_ascii=False)) + "\n")
    args.output.with_suffix(".api.log").write_text(redact("\n".join(logs)))
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
