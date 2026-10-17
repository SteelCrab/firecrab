#!/usr/bin/env python3
"""Real API -> helper -> CLI -> Nginx test on a disposable Linux systemd host."""

import argparse
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import tempfile
import threading
import time


def main(args):
    assert os.geteuid() == 0 and Path("/run/systemd/system").is_dir()
    unit = "firecrab-ingress.service"
    assert subprocess.check_output(["systemctl", "show", "--property=LoadState", "--value", unit], text=True).strip() == "not-found", "refusing existing ingress"

    class Backend(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200); self.end_headers(); self.wfile.write(b"ready")
        def log_message(self, *args):
            pass

    backend = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    threading.Thread(target=backend.serve_forever, daemon=True).start()

    def port():
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0)); return listener.getsockname()[1]

    api_port, ingress_port = port(), port()
    with tempfile.TemporaryDirectory(prefix="fc-pipe-api-", dir="/var/lib") as temporary:
        root = Path(temporary); root.chmod(0o755)
        binaries = root / "bin"; binaries.mkdir()
        for name, source in [("firecrab-api", args.api), ("firecrab-helper", args.helper), ("firecrab", str(Path(args.helper).with_name("firecrab")))]:
            shutil.copyfile(source, binaries / name); (binaries / name).chmod(0o755)
        working = root / "api"; working.mkdir(); os.chown(working, 65534, 65534)
        socket_path = root / "helper.sock"
        environment = dict(os.environ, CONFDIR=str(root / "config"), UNITDIR="/run/systemd/system", FIRECRAB_NET_HELPER_SOCK=str(socket_path), FIRECRAB_NET_HELPER_ALLOWED_UID="65534", FIRECRAB_BIND_ADDR=f"127.0.0.1:{api_port}", FIRECRAB_IMAGE_ROOT=str(working / "images"), FIRECRAB_BOOTSTRAP_WARMUP="0", HOME=str(working))
        helper_log = (root / "helper.log").open("w+")
        api_log = (root / "api.log").open("w+")
        helper_unit = root.name + "-helper.service"
        config_dir = root / "config"; config_dir.mkdir(); config_dir.chmod(0o750); os.chown(config_dir, 0, 65534)
        shared = root / "share"; shared.mkdir()
        source = Path(__file__).resolve().parent.parent
        template = (source / "packaging/systemd/firecrab-helper.service").read_text()
        for key, value in {"@FIRECRAB_GROUP@": "nogroup", "@FIRECRAB_UID@": "65534", "@PREFIX@": str(root), "@LIBDIR@": str(binaries), "@SHAREDIR@": str(shared), "@DATADIR@": str(working), "@CONFDIR@": str(config_dir), "@UNITDIR@": "/run/systemd/system"}.items(): template = template.replace(key, value)
        template = template.replace("RuntimeDirectory=firecrab", "RuntimeDirectory=" + root.name).replace("/run/firecrab ", "/run/" + root.name + " ")
        template = template.replace("[Install]", "Environment=FIRECRAB_NET_HELPER_SOCK=" + str(socket_path) + "\n\n[Install]")
        if profile := os.environ.get("LLVM_PROFILE_FILE"):
            # systemd expands % specifiers; LLVM must receive them literally.
            profile = profile.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%")
            template = template.replace("[Install]", 'Environment="LLVM_PROFILE_FILE=' + profile + '"\n\n[Install]')
        helper_path = Path("/run/systemd/system", helper_unit); helper_path.write_text(template)
        subprocess.run(["systemctl", "daemon-reload"], check=True)
        subprocess.run(["systemctl", "start", helper_unit], check=True)
        helper = None
        api = None

        def start_api():
            def unprivileged():
                os.setgroups([]); os.setgid(65534); os.setuid(65534)
            return subprocess.Popen([str(binaries / "firecrab-api")], cwd=working, env=environment, stdout=api_log, stderr=api_log, preexec_fn=unprivileged)

        def request(method, path, body=None, ingress=False):
            connection = http.client.HTTPConnection("127.0.0.1", ingress_port if ingress else api_port, timeout=30)
            try:
                connection.request(method, path, body=None if body is None else json.dumps(body), headers={"Host": "app.example.com", "Content-Type": "application/json"})
                response = connection.getresponse(); return response.status, response.read()
            finally:
                connection.close()

        def ready():
            for _ in range(100):
                try:
                    if request("GET", "/api/pipe")[0] == 200: return
                except OSError:
                    pass
                time.sleep(.1)
            raise AssertionError("API did not become ready")

        try:
            for _ in range(100):
                if socket_path.exists(): break
                assert subprocess.run(["systemctl", "is-active", "--quiet", helper_unit]).returncode == 0, "helper exited"
                time.sleep(.05)
            os.chown(socket_path, 0, 65534)
            api = start_api(); ready()
            config = {"listen": f"127.0.0.1:{ingress_port}", "routes": [{"host": "app.example.com", "pathPrefix": "/", "upstreams": [f"127.0.0.1:{backend.server_port}"]}], "tunnel": None}
            status, body = request("PUT", "/api/pipe", {"config": config, "credentials": None})
            assert status == 200, body
            snapshot = json.loads(body); original = snapshot["units"][0]["mainPid"]
            assert original > 0 and request("GET", "/", ingress=True) == (200, b"ready")
            assert os.stat(root / "config/pipe/manifest.json").st_uid == 0
            print("PASS unprivileged API applies routes through the sandboxed systemd helper", flush=True)

            rejected = json.loads(json.dumps(config)); rejected["routes"][0]["pathPrefix"] = '/api";return 200;'
            assert request("PUT", "/api/pipe", {"config": rejected, "credentials": None})[0] == 400
            api.terminate(); api.wait(timeout=10)
            for _ in range(10): assert request("GET", "/", ingress=True) == (200, b"ready")
            api = start_api(); ready()
            snapshot = json.loads(request("GET", "/api/pipe")[1])
            assert snapshot["units"][0]["mainPid"] == original and snapshot["config"] == config
            print("PASS real API stop/restart preserves ingress PID, traffic and saved routes", flush=True)
            status, body = request("POST", "/api/pipe/disable")
            assert status == 200 and json.loads(body)["units"][0]["mainPid"] == 0, body
            print("PASS API disable controls the independent unit", flush=True)
        except BaseException:
            for log in [helper_log, api_log]: log.seek(0); print(log.read())
            subprocess.run(["journalctl", "-u", helper_unit, "-n", "35", "--no-pager"])
            raise
        finally:
            for process in [api, helper]:
                if process and process.poll() is None: process.terminate(); process.wait(timeout=10)
            subprocess.run(["systemctl", "stop", helper_unit], capture_output=True)
            helper_path.unlink(missing_ok=True)
            subprocess.run(["systemctl", "disable", "--now", unit], capture_output=True)
            Path("/run/systemd/system", unit).unlink(missing_ok=True)
            subprocess.run(["systemctl", "daemon-reload"], check=True)
            subprocess.run(["systemctl", "reset-failed", unit], capture_output=True)
            backend.shutdown(); backend.server_close()
            helper_log.close(); api_log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True)
    parser.add_argument("--helper", required=True)
    main(parser.parse_args())
