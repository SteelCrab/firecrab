#!/usr/bin/env python3
"""Test fixed-address CLI transactions on a disposable Linux systemd host.

Use real Nginx and systemd with local registration and connector fixtures.
No production token, Cloudflare account, DNS or public connection is used.
"""

import argparse
import fcntl
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import tempfile
import threading
import time


TOKENS = {"a" * 64: "alpha", "b" * 64: "beta", "c" * 64: "native"}
USER_ID = "11111111-1111-4111-8111-111111111111"


def main(args):
    assert os.geteuid() == 0 and Path("/run/systemd/system").is_dir()
    unit_root = Path("/run/systemd/system")
    units = ["firecrab-ingress.service", "firecrab-tunnel.service", "firecrab-address-updater.service", "firecrab-address-lock-test.service"]

    def systemctl(*arguments, check=True):
        return subprocess.run(["systemctl", *arguments], check=check, capture_output=True, text=True)

    for unit in units:
        assert systemctl("show", "--property=LoadState", "--value", unit).stdout.strip() == "not-found", f"refusing existing {unit}"
    for directory in ["/etc/systemd/system", "/run/systemd/system", "/usr/lib/systemd/system"]:
        for path in Path(directory).glob("firecrab-*.service"):
            assert path.name not in units and not path.name.startswith("firecrab-quick-tunnel-"), f"refusing existing {path}"

    class Backend(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", str(len(self.server.body)))
            self.end_headers()
            self.wfile.write(self.server.body)

        def log_message(self, *arguments):
            pass

    class Registry(http.server.BaseHTTPRequestHandler):
        def reply(self, status, body):
            body = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def handle_request(self):
            token = self.headers.get("Authorization", "").removeprefix("Bearer ")
            name = TOKENS.get(token)
            length = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(length)) if length else None
            self.server.events.append((self.command, self.path, body))
            if self.path == "/v1/register":
                assert body == {}, "ownership lookup must not publish a target"
                if self.server.native:
                    return self.reply(404, {"error": "not_found"})
                if self.server.failure:
                    return self.reply(*self.server.failure)
            elif self.path == "/v1/token":
                if not self.server.native:
                    return self.reply(404, {"error": "not_found"})
            if not name:
                return self.reply(401, {"error": "invalid_token"})
            public = f"https://{name}.firecrab.dev"
            if self.path == "/v1/register":
                return self.reply(200, {"name": name, "url": public, "heartbeat_url": public + "/__firecrab/heartbeat", "suspended": False})
            if self.path == "/v1/token":
                return self.reply(200, {"subdomain": name, "publicUrl": public})
            if self.path == "/__firecrab/heartbeat":
                assert self.headers["Host"] == f"{name}.firecrab.dev"
                assert body["tunnel_url"].startswith("https://fixture-")
                self.server.targets[name] = body
                return self.reply(200, {"ok": True, "url": public, "suspended": False})
            assert self.path == f"/v1/addresses/{name}"
            if self.command == "DELETE":
                return self.reply(self.server.delete_status, {})
            assert self.command == "PUT"
            self.server.targets[name] = body
            return self.reply(200, {})

        do_GET = do_POST = do_PUT = do_DELETE = handle_request

        def log_message(self, *arguments):
            pass

    servers = []
    for handler, body in [(Backend, b"first"), (Backend, b"second"), (Registry, None)]:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        server.body = body
        servers.append(server)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    registry = servers[-1]
    registry.events, registry.targets = [], {}
    registry.native, registry.failure, registry.delete_status = False, None, 200
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        ingress_port = reservation.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="fc-address-cli-", dir="/var/lib") as temporary:
        root = Path(temporary)
        root.chmod(0o755)
        binaries = root / "bin"
        binaries.mkdir()
        shutil.copyfile(args.nginx, binaries / "nginx")
        (binaries / "nginx").chmod(0o755)
        shutil.copyfile(args.firecrab, binaries / "firecrab")
        (binaries / "firecrab").chmod(0o755)
        library = root / "lib/pipe"
        library.mkdir(parents=True)
        connector = library / "cloudflared"
        connector.write_text("#!/usr/bin/python3\nimport signal,uuid\nprint('| https://fixture-' + uuid.uuid4().hex + '.trycloudflare.com |', flush=True)\nsignal.pause()\n")
        connector.chmod(0o755)
        source = Path(__file__).resolve().parent.parent
        engine = json.loads((source / "packaging/pipe-engines.json").read_text())["cloudflared"]
        arch = "aarch64" if platform.machine() in ["aarch64", "arm64"] else "x86_64"
        # A root-owned fixture and its matching cached-install marker avoid downloads.
        (library / "cloudflared.json").write_text(json.dumps({"version": engine["version"], "asset_sha256": engine["assets"][arch]["sha256"], "binary_sha256": hashlib.sha256(connector.read_bytes()).hexdigest()}))
        environment = dict(os.environ, CONFDIR=str(root / "config"), UNITDIR=str(unit_root), FIRECRAB_LIBDIR=str(root / "lib"), FIRECRAB_TUNNEL_ENDPOINT=f"http://127.0.0.1:{registry.server_port}", PATH=str(binaries) + ":" + os.environ["PATH"])
        cli = str(binaries / "firecrab")
        manifest = root / "input.json"
        upstreams = [f"127.0.0.1:{server.server_port}" for server in servers[:2]]
        routes = [{"host": "pipe.test", "pathPrefix": "/", "upstreams": [upstreams[0]]}, {"host": "pipe.test", "pathPrefix": "/api", "upstreams": [upstreams[1]]}]
        config = {"listen": f"127.0.0.1:{ingress_port}", "routes": routes, "tunnel": None, "quickTunnels": [{"id": USER_ID, "name": "Unrelated user tunnel", "target": {"type": "apps", "upstreams": [upstreams[0]]}}]}
        manifest.write_text(json.dumps(config))
        store_path = root / "config/pipe/firecrab-addresses.json"
        saved_manifest = root / "config/pipe/manifest.json"
        dropin = unit_root / "firecrab-address-updater.service.d/90-test-coverage.conf"
        release_sync_lock = threading.Event()
        sync_restarted = root / "sync-restarted"
        sync_blocked = root / "sync-blocked"
        sync_trigger = root / "block-first-sync"
        systemctl_binary = shutil.which("systemctl")
        wrapper = binaries / "systemctl"
        wrapper.write_text(f'''#!/usr/bin/python3
import os, pathlib, subprocess, sys, time
command = [{systemctl_binary!r}, *sys.argv[1:]]
trigger = pathlib.Path({str(sync_trigger)!r})
if sys.argv[1:] == ["restart", "firecrab-address-updater.service"] and trigger.exists():
    trigger.unlink()
    subprocess.run(command, check=True)
    pathlib.Path({str(sync_restarted)!r}).touch()
    deadline = time.monotonic() + 5
    while not pathlib.Path({str(sync_blocked)!r}).exists():
        assert time.monotonic() < deadline, "fixture did not block the first sync"
        time.sleep(.01)
    sys.exit(0)
os.execv(command[0], command)
''')
        wrapper.chmod(0o755)

        def block_first_sync():
            while not sync_restarted.exists():
                if release_sync_lock.wait(.01):
                    return
            with open("/run/firecrab-pipe-apply.lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                sync_blocked.touch()
                release_sync_lock.wait(30)

        sync_thread = threading.Thread(target=block_first_sync, daemon=True)
        sync_thread.start()

        def pipe(*arguments, body=None, success=True):
            payload = body if isinstance(body, str) else json.dumps(body) if body is not None else None
            deadline = time.monotonic() + 10
            while True:
                output = subprocess.run([cli, "pipe", *arguments], env=environment, input=payload, capture_output=True, text=True, timeout=90)
                busy = any(message in output.stderr for message in ["Another Firecrab address change is running", "another pipe change is running"])
                if not busy or time.monotonic() >= deadline:
                    break
                time.sleep(.1)
            assert all(token not in output.stdout + output.stderr for token in TOKENS)
            assert (output.returncode == 0) == success, output.stdout + output.stderr
            return output

        def snapshot():
            return json.loads(pipe("inspect").stdout)

        def store():
            return json.loads(store_path.read_text())

        def binding(name):
            return next(value for value in store()["bindings"] if value["subdomain"] == name)

        def wait_binding(name, connected=True):
            # Advance a 30-second updater cycle without exhausting systemd's
            # start budget during the fixture's accelerated lifecycle changes.
            systemctl("reset-failed", units[2])
            systemctl("restart", units[2])
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                values = snapshot()["firecrabTunnels"]
                value = next(entry for entry in values if entry["subdomain"] == name)
                if value["connected"] == connected:
                    return value
                time.sleep(.1)
            raise AssertionError(f"{name} did not become connected={connected}: {value}")

        def pid(unit):
            return int(systemctl("show", "--property=MainPID", "--value", unit).stdout)

        def connector_unit(identity):
            return "firecrab-quick-tunnel-" + identity.replace("-", "") + ".service"

        def ingress(identity, path="/"):
            connection = http.client.HTTPConnection("127.0.0.1", ingress_port, timeout=5)
            try:
                connection.request("GET", path, headers={"Host": "quick-" + identity.replace("-", "") + ".firecrab.internal", "Connection": "close"})
                response = connection.getresponse()
                assert response.status == 200
                return response.read()
            finally:
                connection.close()

        try:
            if profile := os.environ.get("LLVM_PROFILE_FILE"):
                dropin.parent.mkdir()
                escaped = profile.replace("%", "%%").replace('"', '\\"')
                dropin.write_text(f'[Service]\nEnvironment="LLVM_PROFILE_FILE={escaped}"\nBindPaths={Path(profile).parent}\nUMask=0022\n')
            # Address edits use the default engine resolver; bootstrap with it
            # too, including on CI hosts that already have /usr/sbin/nginx.
            pipe("apply", "--config", str(manifest), "--cloudflared", str(connector))
            original = saved_manifest.read_bytes()
            user_pid = pid(connector_unit(USER_ID))
            boot_locks = [root / "boot-apply.lock", root / "boot-address.lock"]
            boot_result = root / "config/pipe/boot-locks.json"
            probe = binaries / "check-boot-locks"
            probe.write_text(f'''#!/usr/bin/python3
import json, pathlib, signal
paths = [pathlib.Path(value) for value in {list(map(str, boot_locks))!r}]
for path in paths:
    with path.open("a"):
        assert path.stat().st_mode & 0o777 == 0o600
result = pathlib.Path({str(boot_result)!r})
result.with_suffix(".tmp").write_text(json.dumps([path.stat().st_ino for path in paths]))
result.with_suffix(".tmp").replace(result)
signal.pause()
''')
            probe.chmod(0o755)
            boot_unit = (source / "packaging/systemd/firecrab-address-updater.service").read_text().replace("@CLI@", str(probe)).replace("@CONFDIR@", str(root / "config"))
            for original_path, private_path in zip(["/run/firecrab-pipe-apply.lock", "/run/firecrab-addresses.lock"], boot_locks):
                boot_unit = boot_unit.replace(original_path, str(private_path))
            (unit_root / units[3]).write_text(boot_unit)
            systemctl("daemon-reload")
            systemctl("start", units[3])
            deadline = time.monotonic() + 5
            while not boot_result.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            inodes = json.loads(boot_result.read_text())
            with boot_locks[0].open("a") as first_lock, boot_locks[1].open("a") as second_lock:
                for lock in [first_lock, second_lock]:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                boot_result.unlink()
                systemctl("restart", units[3])
                deadline = time.monotonic() + 5
                while not boot_result.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                assert json.loads(boot_result.read_text()) == inodes
            systemctl("stop", units[3])
            print("PASS updater sandbox creates missing private locks and preserves held lock inodes on restart", flush=True)
            pipe("firecrab-manage", body="x" * 4097, success=False)
            pipe("firecrab-manage", body={"action": "connect", "token": "f" * 64, "target": {"type": "apps", "upstreams": upstreams}}, success=False)
            for failure in [(429, {}), (503, {}), (200, {"name": "alpha", "url": "https://alpha.firecrab.dev", "heartbeat_url": "https://other.invalid/", "suspended": False})]:
                registry.failure = failure
                pipe("firecrab-manage", body={"action": "connect", "token": "a" * 64, "target": {"type": "apps", "upstreams": upstreams}}, success=False)
            registry.failure = None
            assert saved_manifest.read_bytes() == original and not store_path.exists()
            print("PASS invalid tokens, unavailable registration and unsafe replies preserve local configuration", flush=True)

            token_file = root / "token"
            token_file.write_text("a" * 64)
            token_file.chmod(0o600)
            sync_trigger.touch()
            try:
                pipe("address", "connect", "--token-file", str(token_file), "--app", upstreams[0], "--app", upstreams[1])
                assert sync_blocked.exists() and binding("alpha")["target"] == {"type": "apps", "upstreams": upstreams}
            finally:
                release_sync_lock.set()
                sync_thread.join(timeout=5)
            print("PASS a committed address succeeds when its immediate sync is blocked", flush=True)
            first = wait_binding("alpha")
            identity = first["connectorId"]
            assert first["target"] == {"type": "apps", "upstreams": upstreams}
            assert {ingress(identity) for _ in range(6)} == {b"first", b"second"}
            assert store_path.stat().st_mode & 0o777 == 0o600
            assert store()["protocol"] == "register_v1"
            assert all(token not in json.dumps(snapshot()) for token in TOKENS)
            print("PASS dashboard token connects a private VM app pool; status omits protected ownership secrets", flush=True)

            unprivileged = subprocess.run(["runuser", "-u", "nobody", "--", cli, "pipe", "status"], env=environment, capture_output=True, text=True, timeout=10)
            assert unprivileged.returncode == 0 and "Nginx" in unprivileged.stdout, unprivileged.stderr
            with open("/run/firecrab-addresses.lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                original_store = store_path.read_bytes()
                try:
                    store_path.write_text("corrupt JSON")
                    degraded = snapshot()
                    assert degraded.get("firecrabTunnels", []) == [] and degraded["config"]["routes"] == routes
                    assert degraded["units"][0]["mainPid"] > 0
                finally:
                    store_path.write_bytes(original_store)
            assert snapshot()["firecrabTunnels"][0]["subdomain"] == "alpha"
            print("PASS private or corrupt address stores preserve Pipe status and readable stores retain addresses", flush=True)

            lookup_count = sum(event[1] == "/v1/register" for event in registry.events)
            pipe("address", "connect", "alpha", "--nginx", "pipe.test")
            routed = wait_binding("alpha")
            assert routed["connectorId"] == identity
            assert ingress(identity) == b"first" and ingress(identity, "/api") == b"second"
            assert sum(event[1] == "/v1/register" for event in registry.events) == lookup_count
            assert json.loads(saved_manifest.read_text())["routes"] == routes
            assert pid(connector_unit(USER_ID)) == user_pid
            print("PASS saved-token retargeting preserves address, connector identity, user tunnel and Nginx paths", flush=True)

            assert "Remove the Firecrab address" in pipe("quick-manage", body={"action": "delete", "id": identity}, success=False).stderr
            assert "Edit the Firecrab address" in pipe("quick-manage", body={"action": "save", "id": identity, "name": "wrong owner", "target": {"type": "apps", "upstreams": upstreams}}, success=False).stderr
            removed = json.loads(saved_manifest.read_text())
            removed["quickTunnels"] = [entry for entry in removed["quickTunnels"] if entry["id"] != identity]
            manifest.write_text(json.dumps(removed))
            assert "Remove Firecrab address" in pipe("apply", "--config", str(manifest), success=False).stderr
            assert binding("alpha")["connectorId"] == identity and pid(connector_unit(identity)) > 0
            print("PASS direct connector edits, deletes and destructive manifest applies cannot orphan a fixed address", flush=True)

            pipe("firecrab-manage", body={"action": "connect", "token": "b" * 64, "target": {"type": "apps", "upstreams": [upstreams[0]]}})
            peer = wait_binding("beta")
            systemctl("stop", connector_unit(identity))
            wait_binding("alpha", False)
            assert binding("beta")["connected"]
            systemctl("start", connector_unit(identity))
            wait_binding("alpha")
            assert "tunnel_url" in registry.targets["alpha"]
            pipe("address", "delete", "alpha")
            assert all(entry["subdomain"] != "alpha" for entry in store()["bindings"])
            assert pid(connector_unit(identity)) == 0
            assert pid(connector_unit(peer["connectorId"])) > 0 and pid(connector_unit(USER_ID)) == user_pid
            pipe("address", "delete", "beta")
            assert not store()["bindings"] and pid(units[2]) == 0
            print("PASS source stop/start and local deletion preserve peer and user connections", flush=True)

            registry.native = True
            token_file.write_text("c" * 64)
            pipe("address", "connect", "--token-file", str(token_file), "--nginx", "pipe.test")
            wait_binding("native")
            assert store()["protocol"] == "addresses_v1"
            assert registry.targets["native"]["enabled"]
            registry.delete_status = 403
            pipe("address", "delete", "native", success=False)
            assert binding("native")["token"] == "c" * 64
            registry.delete_status = 410
            pipe("address", "delete", "native")
            assert not store()["bindings"] and pid(units[2]) == 0
            pipe("address", "connect", "missing", "--nginx", "pipe.test", success=False)
            pipe("address", "delete", "missing", success=False)
            print("PASS standalone API fallback retains ownership after rejection and permits revoked-token cleanup", flush=True)
        except BaseException:
            print(subprocess.run(["journalctl", "--no-pager", "-n", "40", "-u", units[0], "-u", units[2], "-u", units[3]], capture_output=True, text=True).stdout)
            raise
        finally:
            release_sync_lock.set()
            sync_thread.join(timeout=5)
            owned = [path for path in unit_root.glob("firecrab-*.service") if str(root) in path.read_text()]
            systemctl("disable", "--now", *(path.name for path in owned), check=False)
            if any(path.name == units[0] for path in owned):
                systemctl("clean", "--what=state", units[0], check=False)
            for path in owned:
                path.unlink()
            dropin.unlink(missing_ok=True)
            if dropin.parent.exists():
                dropin.parent.rmdir()
            systemctl("daemon-reload", check=False)
            if owned:
                systemctl("reset-failed", *(path.name for path in owned), check=False)
            for server in servers:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecrab", required=True)
    parser.add_argument("--nginx", required=True)
    main(parser.parse_args())
