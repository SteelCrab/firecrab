#!/usr/bin/env python3
"""Disposable TLS database/Kafka QA. Owns and cleans only this run's containers.

Pass a Linux firecrab-api unit-test executable compiled on this checkout.
All credentials and certificate keys stay in a private temporary directory.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
from urllib.parse import quote
import uuid

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path, required=True)
    parser.add_argument("--profiles", type=Path)
    args = parser.parse_args()
    binary = args.test_binary.resolve(strict=True)
    prefix = "qa-db-services-" + uuid.uuid4().hex[:10]
    containers = []
    passwords = [secrets.token_hex(24)]
    cleanup_errors = []

    def scrub(text):
        for password in passwords:
            text = text.replace(password, "[REDACTED]")
        return text

    def run(command, *, quiet=False, data=None):
        result = subprocess.run(command, input=data, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=300)
        if not quiet:
            print(scrub(result.stdout), end="", flush=True)
        if result.returncode:
            if quiet: print(scrub(result.stdout[-2000:]),flush=True)
            raise RuntimeError("QA command failed: " + Path(command[0]).name)
        return result.stdout

    def ready(name, command):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if subprocess.run(["docker", "exec", name, *command], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
                return
            time.sleep(1)
        raise RuntimeError("QA database readiness timed out")

    def start(name, image, environment):
        containers.append(name)  # Cleanup includes unknown docker-run outcomes.
        path = temporary / (name + ".env")
        path.write_text("".join(f"{key}={value}\n" for key, value in environment.items()))
        path.chmod(0o600)
        run(["docker", "run", "-d", "--name", name, "--network", prefix, "--env-file", str(path), image], quiet=True)

    try:
        with tempfile.TemporaryDirectory(prefix=prefix) as directory:
            temporary = Path(directory)
            run(["docker", "build", "-q", "-t", "firecrab-database-services-qa", "-f", str(ROOT / "scripts/database-services-qa.Dockerfile"), str(ROOT)], quiet=True)
            run(["docker", "network", "create", prefix], quiet=True)
            mount = ["-v", f"{binary.parent}:/tests:ro", "-v", f"{temporary}:/qa"]
            if args.profiles:
                args.profiles.mkdir(parents=True, exist_ok=True)
                mount += ["-v", f"{args.profiles.resolve()}:/profiles", "-e", "LLVM_PROFILE_FILE=/profiles/database-services-%p-%m.profraw"]
            base = ["docker", "run", "--rm", "--init", "--network", prefix, *mount]
            run([*base, "--user", f"{os.getuid()}:{os.getgid()}", "-e", "FIRECRAB_KAFKA_EXPORT=/qa/kafka.json", "-e", f"FIRECRAB_KAFKA_QA_PREFIX={prefix}-kafka", "firecrab-database-services-qa", f"/tests/{binary.name}", "export_managed_kafka_fixture", "--ignored", "--nocapture"])
            cluster = json.loads((temporary / "kafka.json").read_text())
            # Never print the cluster object: it contains private TLS keys.
            pg, mysql = prefix + "-postgres", prefix + "-mysql"
            password = passwords[0]
            start(pg, "postgres:17", {"POSTGRES_USER": "qa", "POSTGRES_PASSWORD": password, "POSTGRES_DB": "firecrab_dev"})
            start(mysql, "mysql:8.4", {"MYSQL_ROOT_PASSWORD": password, "MYSQL_ROOT_HOST": "%", "MYSQL_DATABASE": "firecrab_dev"})
            probes = {pg: ["env","PGPASSWORD="+password,"psql","-h127.0.0.1","-U","qa","-d","firecrab_dev","-c","SELECT 1"], mysql: ["env", "MYSQL_PWD=" + password, "mysql","--protocol=TCP","-h127.0.0.1", "-uroot","-Dfirecrab_dev","-e","SELECT 1","--silent"]}
            for name, command in probes.items():
                ready(name, command)
            log = temporary / "certificates.log"
            commands = [
                ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-keyout", str(temporary / "ca.key"), "-out", str(temporary / "ca.pem"), "-subj", "/CN=Firecrab disposable service QA", "-addext", "basicConstraints=critical,CA:TRUE"],
                ["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout", str(temporary / "server.key"), "-out", str(temporary / "server.csr"), "-subj", "/CN=" + pg],
            ]
            (temporary / "extensions").write_text(f"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:{pg},DNS:{mysql},IP:127.0.0.1\n")
            commands.append(["openssl", "x509", "-req", "-in", str(temporary / "server.csr"), "-CA", str(temporary / "ca.pem"), "-CAkey", str(temporary / "ca.key"), "-CAcreateserial", "-days", "1", "-extfile", str(temporary / "extensions"), "-out", str(temporary / "server.crt")])
            with log.open("w") as stream:
                for command in commands:
                    if subprocess.run(command, stdout=stream, stderr=stream).returncode:
                        raise RuntimeError("QA certificate generation failed")
            for name in [pg, mysql]:
                for filename in ["server.crt", "server.key", "ca.pem"]:
                    run(["docker", "cp", str(temporary / filename), name + ":/tmp/qa-" + filename], quiet=True)
            run(["docker", "exec", "-u", "0", pg, "sh", "-c", "cp /tmp/qa-server.crt /var/lib/postgresql/data/server.crt; cp /tmp/qa-server.key /var/lib/postgresql/data/server.key; chown postgres:postgres /var/lib/postgresql/data/server.*; chmod 600 /var/lib/postgresql/data/server.key"], quiet=True)
            run(["docker", "exec", pg, "psql", "-U", "qa", "-d", "firecrab_dev", "-v", "ON_ERROR_STOP=1", "-c", "ALTER SYSTEM SET ssl=on"], quiet=True)
            run(["docker", "exec", pg, "psql", "-U", "qa", "-d", "firecrab_dev", "-c", "SELECT pg_reload_conf()"], quiet=True)
            run(["docker", "exec", "-u", "0", mysql, "sh", "-c", "cp /tmp/qa-server.crt /var/lib/mysql/server-cert.pem; cp /tmp/qa-server.key /var/lib/mysql/server-key.pem; cp /tmp/qa-ca.pem /var/lib/mysql/ca.pem; chown mysql:mysql /var/lib/mysql/server-cert.pem /var/lib/mysql/server-key.pem /var/lib/mysql/ca.pem; chmod 600 /var/lib/mysql/server-key.pem"], quiet=True)
            run(["docker", "exec", "-e", "MYSQL_PWD=" + password, mysql, "mysql", "-uroot", "-e", "ALTER INSTANCE RELOAD TLS"], quiet=True)
            ca = (temporary / "ca.pem").read_text()
            inputs = [{"name": "QA " + engine, "provider": "local", "uri": f"{engine}://{user}:{quote(password)}@{host}/firecrab_dev", "tlsMode": "verify_full", "caCertificate": ca, "writeEnabled": True} for engine, user, host in [("postgresql", "qa", pg), ("mysql", "root", mysql)]]
            (temporary / "input.json").write_text(json.dumps(inputs))
            (temporary / "input.json").chmod(0o600)
            for broker in cluster["brokers"]:
                start(broker["ipv4"], "apache/kafka:4.3.1", broker["env"])
            deadline = time.monotonic() + 120
            while not all("Kafka Server started" in run(["docker", "logs", broker["ipv4"]], quiet=True) for broker in cluster["brokers"]):
                if time.monotonic() > deadline:
                    raise RuntimeError("QA Kafka startup timed out")
                time.sleep(1)
            tests = [*base, "-e", "FIRECRAB_DB_SERVICES_QA_JSON=/qa/input.json", "-e", "FIRECRAB_KAFKA_FIXTURE=/qa/kafka.json", "firecrab-database-services-qa", f"/tests/{binary.name}"]
            for name in ["database_services_real_engines", "native_backups_migrations_and_restore", "database_services_api_real_engines"]:
                run([*tests, name, "--ignored", "--nocapture"])
            gate = temporary / "restart-gate"
            gate.mkdir()
            command = [*base, "-e", "FIRECRAB_DB_SERVICES_QA_JSON=/qa/input.json", "-e", "FIRECRAB_KAFKA_FIXTURE=/qa/kafka.json", "-e", "FIRECRAB_QA_RESTART_GATE=/qa/restart-gate", "firecrab-database-services-qa", f"/tests/{binary.name}", "durable_events_real_kafka", "--ignored", "--nocapture"]
            process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            try:
                deadline, completed = time.monotonic() + 480, set()
                while process.poll() is None:
                    for engine, database in [("postgres", pg), ("mysql", mysql)]:
                        if engine in completed or not (gate / (engine + ".ready")).exists():
                            continue
                        print("Restarting disposable " + engine + " and three TLS Kafka brokers", flush=True)
                        run(["docker", "restart", "-t", "5", *[broker["ipv4"] for broker in cluster["brokers"]], database], quiet=True)
                        ready(database, probes[database])
                        time.sleep(8)
                        (gate / (engine + ".continue")).write_text("ready")
                        completed.add(engine)
                    if time.monotonic() > deadline:
                        raise RuntimeError("QA restart recovery timed out")
                    time.sleep(.2)
                print(scrub(process.stdout.read()), end="", flush=True)
                if process.returncode:
                    raise RuntimeError("Kafka event recovery QA failed")
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=15)
            run(["docker", "run", "--rm", "--init", "--network", prefix, "-v", f"{ROOT}:/work:ro", "-v", f"{temporary}:/qa:ro", "-w", "/work/examples/database-events", "-e", "FIRECRAB_DB_SERVICES_QA_JSON=/qa/input.json", "node:22-bookworm-slim", "node", "--test", "receiver.test.mjs", "integration.test.mjs"])
    finally:
        for name in reversed(containers):
            if subprocess.run(["docker", "rm", "-fv", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                if subprocess.run(["docker", "inspect", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
                    cleanup_errors.append(name)
        subprocess.run(["docker", "network", "rm", prefix], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if cleanup_errors:
            raise RuntimeError("Disposable QA container cleanup is incomplete")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        # No subprocess command lines or raw database diagnostics contain output.
        print("Database service QA failed: " + (str(error) if isinstance(error, RuntimeError) else type(error).__name__), flush=True)
        raise SystemExit(1)
