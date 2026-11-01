#!/usr/bin/env python3
"""Exercise the disposable PostgreSQL security image before MicroVM import."""
import json
import subprocess
import sys
import time
import uuid

PASSWORD = "firecrab-db-qa-only"
BAD_PASSWORD = "incorrect-qa-password"


def docker(*args):
    return subprocess.run(["docker", *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30)


def main():
    if len(sys.argv) != 2 or not sys.argv[1].startswith("firecrab-postgres-security-qa:"):
        raise SystemExit("usage: test-security-postgres-image.py firecrab-postgres-security-qa:TAG")
    name = "firecrab-db-security-qa-" + uuid.uuid4().hex[:12]
    started = False
    try:
        result = docker("run", "-d", "--name", name, "-e", "POSTGRES_PASSWORD=" + PASSWORD, sys.argv[1])
        if result.returncode:
            raise RuntimeError("could not start disposable PostgreSQL fixture")
        started = True
        for _ in range(120):
            connection = docker("exec", "-e", "PGPASSWORD=" + PASSWORD, name, "psql", "-w", "-h", "127.0.0.1", "-U", "postgres", "-d", "postgres", "-c", "SELECT 1")
            health = docker("exec", name, "busybox", "wget", "-q", "-O", "-", "http://127.0.0.1:8080/cgi-bin/health")
            if connection.returncode == 0 and health.returncode == 0 and health.stdout.strip() == "healthy":
                break
            time.sleep(1)
        else:
            raise RuntimeError("PostgreSQL fixture failed its authenticated connection or health check")
        for _ in range(10):
            denied = docker("exec", "-e", "PGPASSWORD=" + BAD_PASSWORD, name, "psql", "-w", "-h", "127.0.0.1", "-U", "postgres", "-d", "postgres", "-c", "SELECT 1")
            if denied.returncode == 0 or "password authentication failed" not in denied.stderr:
                raise RuntimeError("PostgreSQL fixture did not reject invalid credentials")
        for _ in range(30):
            logs = docker("logs", name)
            failures = 0
            for line in (logs.stdout + "\n" + logs.stderr).splitlines():
                try:
                    value = json.loads(line)
                except ValueError:
                    continue
                if value.get("error_severity") == "FATAL" and value.get("state_code") == "28P01":
                    failures += 1
            if failures >= 10:
                break
            time.sleep(1)
        else:
            raise RuntimeError("PostgreSQL authentication failures were not forwarded as JSON")
        print("PASS real PostgreSQL connection, health and authentication log fixture")
    finally:
        if started:
            result = docker("rm", "-f", name)
            if result.returncode:
                raise RuntimeError("could not remove disposable PostgreSQL fixture")


if __name__ == "__main__":
    main()
