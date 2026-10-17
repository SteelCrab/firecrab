#!/usr/bin/env python3
"""Exercise pipe apply/rollback with real systemd on a disposable Linux host.

Refuses existing Firecrab traffic units. The connector is a lifecycle test double;
its config validation delegates to real cloudflared. No Cloudflare account needed.
"""

import argparse
import http.client
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import uuid


def main(args):
    assert os.geteuid() == 0, "requires root on a disposable systemd host"
    units = ["firecrab-ingress.service", "firecrab-tunnel.service"]
    unit_root = Path("/run/systemd/system")
    assert unit_root.is_dir()

    def systemctl(*arguments, check=True):
        return subprocess.run(["systemctl", *arguments], check=check, capture_output=True, text=True)

    for unit in units:
        assert systemctl("show", "--property=LoadState", "--value", unit).stdout.strip() == "not-found", f"refusing existing {unit}"
        for directory in ["/etc/systemd/system", "/run/systemd/system", "/usr/lib/systemd/system"]:
            assert not (Path(directory) / unit).exists(), f"refusing existing {unit}"

    class Backend(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = self.server.body
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    servers = []
    for body in [b"first", b"second"]:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend)
        server.body = body
        threading.Thread(target=server.serve_forever, daemon=True).start()
        servers.append(server)
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]

    def request(body):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
        try:
            connection.request("GET", "/", headers={"Host": "app.example.com"})
            response = connection.getresponse()
            assert (response.status, response.read()) == (200, body)
        finally:
            connection.close()

    def identities():
        return [int(systemctl("show", "--property=MainPID", "--value", unit).stdout) for unit in units]

    with tempfile.TemporaryDirectory(prefix="fc-pipe-cli-", dir="/var/lib") as temporary:
        root = Path(temporary)
        root.chmod(0o755)
        engines = root / "engines"
        engines.mkdir()
        for name, source in [("nginx", args.nginx), ("cloudflared-real", args.cloudflared)]:
            shutil.copyfile(source, engines / name)
            (engines / name).chmod(0o755)
        connector = engines / "cloudflared"
        connector.write_text('''#!/usr/bin/python3
import json, os, socket, sys, time
if "validate" in sys.argv:
    os.execv(os.path.join(os.path.dirname(__file__), "cloudflared-real"), ["cloudflared", *sys.argv[1:]])
config=json.load(open(sys.argv[sys.argv.index("--config")+1]))
assert json.load(open(config["credentials-file"]))["TunnelSecret"] == "private-fixture-secret"
address=os.environ["NOTIFY_SOCKET"]
if address.startswith("@"): address="\\0"+address[1:]
notify=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM)
notify.connect(address)
notify.send(b"READY=1")
while True: time.sleep(10)
''')
        connector.chmod(0o755)
        environment = dict(os.environ, CONFDIR=str(root / "config"), UNITDIR=str(unit_root))
        cli = str(Path(args.firecrab).resolve())
        manifest = root / "pipe.json"
        credentials = root / "credentials.json"
        tunnel_id = "11111111-1111-4111-8111-111111111111"
        secret = {"AccountTag": "fixture", "TunnelID": tunnel_id, "TunnelSecret": "private-fixture-secret"}
        credentials.write_text(json.dumps(secret))
        credentials.chmod(0o600)
        config = {"listen": f"127.0.0.1:{port}", "routes": [{"host": "app.example.com", "upstreams": [f"127.0.0.1:{servers[0].server_port}"]}], "tunnel": {"id": tunnel_id, "credentialsFile": str(credentials)}}
        overrides = ["--nginx", str(engines / "nginx"), "--cloudflared", str(connector)]

        def pipe(*arguments, input=None, success=True):
            output = subprocess.run([cli, "pipe", *arguments], env=environment, input=input, capture_output=True, text=True, timeout=90)
            assert "private-fixture-secret" not in output.stdout + output.stderr
            if success:
                assert output.returncode == 0, output.stdout + output.stderr
            else:
                assert output.returncode != 0, "expected apply rejection"
            return output

        def apply(success=True):
            manifest.write_text(json.dumps(config))
            return pipe("apply", "--config", str(manifest), *overrides, success=success)

        control = "fc-pipe-control-" + uuid.uuid4().hex[:10] + ".service"
        try:
            apply()
            original = identities()
            assert all(original)
            request(b"first")
            stored = root / "config/pipe"
            assert (stored / "secrets/tunnel-credentials.json").stat().st_mode & 0o777 == 0o600
            pipe("status")
            (unit_root / control).write_text("[Service]\nExecStart=/bin/sleep infinity\n")
            systemctl("daemon-reload")
            systemctl("start", control)
            systemctl("stop", control)
            for _ in range(10):
                request(b"first")
            systemctl("start", control)
            assert identities() == original
            print("PASS CLI installs independent units and preserves traffic across control-plane stop/start", flush=True)

            config["routes"][0]["upstreams"] = [f"127.0.0.1:{servers[1].server_port}"]
            # Exercise the stdin transport used by native macOS/Windows commands.
            bundle = {"config": config, "credentials": secret}
            pipe("apply-bundle", *overrides, input=json.dumps(bundle))
            request(b"second")
            assert identities() == original
            print("PASS streamed apply changes routes without replacing either process", flush=True)

            previous = (stored / "ingress.conf").read_bytes()
            with socket.socket() as occupied:
                occupied.bind(("127.0.0.1", 0))
                occupied.listen()
                config["listen"] = f"127.0.0.1:{occupied.getsockname()[1]}"
                output = apply(success=False)
                assert "previous pipe configuration restored" in output.stderr
            assert (stored / "ingress.conf").read_bytes() == previous
            assert identities() == original
            request(b"second")
            print("PASS failed listener reload restores persisted config and preserves the connector", flush=True)

            config["listen"] = f"127.0.0.1:{port}"
            del config["tunnel"]
            apply()
            request(b"second")
            assert identities()[0] == original[0] and identities()[1] == 0
            assert not (stored / "secrets/tunnel-credentials.json").exists()
            assert not (unit_root / units[1]).exists()
            pipe("disable")
            assert identities() == [0, 0]
            print("PASS removing a tunnel deletes managed credentials; disable stops ingress", flush=True)
        except BaseException:
            print(subprocess.run(["journalctl", "--no-pager", "-n", "40", "-u", units[0], "-u", units[1]], capture_output=True, text=True).stdout)
            raise
        finally:
            systemctl("disable", "--now", *units, check=False)
            systemctl("stop", control, check=False)
            systemctl("clean", "--what=state", units[0], check=False)
            for unit in [*units, control]:
                (unit_root / unit).unlink(missing_ok=True)
            systemctl("daemon-reload", check=False)
            systemctl("reset-failed", *units, control, check=False)
            for server in servers:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firecrab", required=True)
    parser.add_argument("--nginx", required=True)
    parser.add_argument("--cloudflared", required=True)
    main(parser.parse_args())
