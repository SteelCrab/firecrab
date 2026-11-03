#!/usr/bin/env python3
"""Actual privileged CLI maintenance journal; temporary files, no service/VM changes."""
import json
import os
from pathlib import Path
import select
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def main():
    if sys.platform != "linux" or os.geteuid() != 0:
        raise SystemExit("run only inside the dedicated root Linux test container")
    binary = Path(sys.argv[1]).resolve(strict=True)
    machine = Path("/etc/machine-id")
    if not machine.exists() or not machine.read_text().strip():
        machine.write_text(uuid.uuid4().hex + "\n")
    with tempfile.TemporaryDirectory(prefix="fc-native-iac-") as temporary:
        root = Path(temporary)
        home = root / "conf"
        home.mkdir(mode=0o700)
        api_configuration=b"FIRECRAB_BIND_ADDR=127.0.0.1:5523\n"
        (home / "api.env").write_bytes(api_configuration)
        units = root / "units"
        units.mkdir()
        for unit in ["firecrab-api.service", "firecrab-helper.service"]:
            (units / unit).write_text("[Service]\nExecStart=/fixture/never-executed\n")
        fixture_bin = root / "bin"
        fixture_bin.mkdir()
        control_log = root / "control.log"
        systemctl = fixture_bin / "systemctl"
        systemctl.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$NATIVE_CONTROL_LOG"\n')
        systemctl.chmod(0o755)
        prefix=root / "install"
        (prefix / "bin").mkdir(parents=True)
        (prefix / "bin/firecrab").symlink_to(binary)
        env = dict(os.environ, CONFDIR=str(home), UNITDIR=str(units),
                   PREFIX=str(prefix),FIRECRAB_LIBDIR=str(prefix / "lib/firecrab"),DATADIR=str(root / "data"),
                   PATH=str(fixture_bin) + os.pathsep + os.environ["PATH"],
                   NATIVE_CONTROL_LOG=str(control_log),
                   FIRECRAB_API="https://unreachable.invalid:1",
                   FIRECRAB_TLS_CERT=str(root / "missing-cert"),
                   FIRECRAB_TLS_KEY=str(root / "missing-key"),
                   FIRECRAB_TLS_CA=str(root / "missing-ca"))

        def command(arguments, success=True):
            result = subprocess.run([str(binary), "iac", *arguments], env=env,
                                    capture_output=True, timeout=15)
            assert (result.returncode == 0) == success, (arguments, result.stderr.decode())
            return result

        def disable(success):
            result = subprocess.run([str(binary), "service", "disable"], env=env,
                                    capture_output=True, timeout=15)
            assert (result.returncode == 0) == success, result.stderr.decode()

        def rpc(request):
            result=subprocess.run([str(binary),"iac","native-broker"],env=env,
                                  input=json.dumps({"version":1,"request":request}).encode(),
                                  capture_output=True,timeout=15,check=True)
            return json.loads(result.stdout)

        operation = str(uuid.uuid4())
        request = ["native-maintenance", "--operation-id", operation,
                   "--action", "restart", "--input-sha256", "a" * 64]

        def start(arguments, expected_operation=None):
            child = subprocess.Popen([str(binary), "iac", *arguments], env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE)
            try:
                assert select.select([child.stdout], [], [], 10)[0], "missing native admission"
                line = child.stdout.readline()
                assert line, child.stderr.read().decode()
                reply = json.loads(line)
                assert reply["operationId"] == (expected_operation or operation)
                return child, reply
            except BaseException:
                child.kill()
                child.wait(timeout=5)
                raise

        original = json.loads(command(["native-identity"]).stdout)
        command(["native-write-allowed"])
        child, ready = start(request)
        try:
            assert ready["state"] == "ready"
            command(["native-write-allowed"], success=False)
            competing = list(request)
            competing[2] = str(uuid.uuid4())
            command(competing, success=False)
            disable(success=False)
            assert not control_log.exists()
            child.stdin.close()  # Lost installer/parent. EOF is not successful completion.
            assert child.wait(timeout=10) != 0
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        command(["native-write-allowed"], success=False)
        command(competing, success=False)
        disable(success=False)
        assert not control_log.exists()
        wrong = list(request)
        wrong[-1] = "b" * 64
        command(wrong, success=False)
        child, resumed = start(request)
        try:
            assert resumed["state"] == "ready"
            child.stdin.write(b"complete\n")
            child.stdin.close()
            assert child.wait(timeout=10) == 0, child.stderr.read().decode()
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        command(["native-write-allowed"])
        repeated = json.loads(command(request).stdout)
        assert repeated["state"] == "completed"
        ledger = json.loads((home / "iac-native/ownership.json").read_text())
        assert len(ledger["maintenance"]) == 1
        assert ledger["maintenance"][operation]["completed"] is True
        assert ledger["owners"] == {} and ledger["outcomes"] == {}
        disable(success=True)
        assert control_log.read_text().splitlines() == [
            "show --property=LoadState --value firecrab-helper.service",
            "disable firecrab-helper.service firecrab-api.service"]
        finished = json.loads((home / "iac-native/ownership.json").read_text())
        assert len(finished["maintenance"]) == 2
        assert all(entry["completed"] for entry in finished["maintenance"].values())
        assert original == json.loads(command(["native-identity"]).stdout)
        assert (home / "iac-native").stat().st_mode & 0o777 == 0o700
        assert (home / "iac-native/ownership.json").stat().st_mode & 0o777 == 0o600
        identity=rpc({"action":"identity"})
        assert identity["kinds"]==["host_configuration"]
        endpoint=str(uuid.uuid4())
        resource={"kind":"host_configuration","id":original["installationId"]}
        observed=rpc({"action":"observe","endpoint_id":endpoint,"resource":resource})["observation"]
        assert observed["exists"] and not observed["busy"]
        state_owner={"manager":"terraform","id":str(uuid.uuid4()),"key":"firecrab_host_configuration.host"}
        owner_change={"action":"change_owner","request":{
            "target":observed["target"],"configurationSha256":observed["configurationSha256"],
            "change":{"operationId":str(uuid.uuid4()),"expectedEndpointId":endpoint,
                      "resource":resource,"expectedGeneration":0,"expectedOwner":None,"owner":state_owner}}}
        acknowledged=rpc(owner_change)
        assert acknowledged["receipt"]["ownership"]["generation"]==1
        assert (home / "api.env").read_bytes()==api_configuration
        before=control_log.read_bytes()
        disable(success=False)
        assert control_log.read_bytes()==before
        # Actual standalone installer must stop before any systemd or file-removal effect.
        rejected=subprocess.run(["bash","install.sh","--uninstall"],env=env,
                                capture_output=True,timeout=15)
        assert rejected.returncode != 0
        assert control_log.read_bytes()==before
        assert (home / "api.env").read_bytes()==api_configuration
        assert (prefix / "bin/firecrab").exists()
        (home / "api.env").unlink()  # Explicit external-deletion fault in the temporary fixture.
        assert rpc(owner_change)==acknowledged
        (home / "api.env").write_bytes(api_configuration)
        change_id=owner_change["request"]["change"]["operationId"]
        rpc({"action":"confirm_owner","endpoint_id":endpoint,"operation_id":change_id})
        release=json.loads(json.dumps(owner_change))
        release["request"]["change"].update(operationId=str(uuid.uuid4()),expectedGeneration=1,
                                              expectedOwner=state_owner,owner=None)
        released=rpc(release)
        assert released["receipt"]["ownership"]["generation"]==2
        disable(success=False)
        rpc({"action":"confirm_owner","endpoint_id":endpoint,
             "operation_id":release["request"]["change"]["operationId"]})
        disable(success=True)
        assert (home / "api.env").read_bytes()==api_configuration
        # Execute the actual installer guard functions, without dispatching installation.
        installer_source=Path("install.sh").read_text()
        installer_functions,dispatch=installer_source.rsplit('\ncase "$MODE" in\n',1)
        assert "do_install" in dispatch and "do_uninstall" in dispatch
        guard_script=installer_functions+"\nrequire_sudo_ticket\nnative_maintenance_begin install\nnative_maintenance_complete\n"
        before=control_log.read_bytes()
        subprocess.run(["bash","-c",guard_script],env=env,capture_output=True,timeout=15,check=True)
        assert control_log.read_bytes()==before
        borrowed_operation=str(uuid.uuid4())
        borrowed_request=["native-maintenance","--operation-id",borrowed_operation,
                          "--action","install","--input-sha256","c"*64]
        child,_=start(borrowed_request,borrowed_operation)
        try:
            borrowed_env=dict(env,FIRECRAB_NATIVE_MAINTENANCE_OPERATION_ID=borrowed_operation,
                              FIRECRAB_NATIVE_MAINTENANCE_INPUT_SHA256="c"*64)
            subprocess.run(["bash","-c",guard_script],env=borrowed_env,capture_output=True,timeout=15,check=True)
            pending=json.loads((home / "iac-native/ownership.json").read_text())
            assert not pending["maintenance"][borrowed_operation]["completed"]
            wrong_env=dict(borrowed_env,FIRECRAB_NATIVE_MAINTENANCE_INPUT_SHA256="d"*64)
            wrong=subprocess.run(["bash","-c",guard_script],env=wrong_env,capture_output=True,timeout=15)
            assert wrong.returncode != 0
            child.stdin.write(b"complete\n")
            child.stdin.close()
            assert child.wait(timeout=10)==0,child.stderr.read().decode()
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        assert control_log.read_bytes()==before
        # Start the actual native TLS server, using temporary certificates from the real CA CLI.
        ca=root / "ca"
        subprocess.run([str(binary),"ca","init","--dir",str(ca)],env=env,
                       capture_output=True,timeout=15,check=True)

        def certificate(identity, server=False):
            args=[str(binary),"ca","issue-host-cert",identity,"--dir",str(ca),"--days","1"]
            if server:
                args.extend(["--san","127.0.0.1"])
            result=subprocess.run(args,env=env,capture_output=True,timeout=15,check=True)
            return dict(line.split(": ",1) for line in result.stdout.decode().splitlines())

        native_certificate=certificate(original["installationId"],server=True)
        api_certificate=certificate(endpoint)
        sibling_certificate=certificate(str(uuid.uuid4()))
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1",0))
            port=reservation.getsockname()[1]
        server=subprocess.Popen([str(binary),"iac","native-serve","--bind",f"127.0.0.1:{port}",
                                 "--endpoint-id",endpoint,"--cert",native_certificate["Certificate"],
                                 "--key",native_certificate["Private key"],
                                 "--ca",native_certificate["CA certificate"]],
                                env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            deadline=time.monotonic()+10
            while True:
                assert server.poll() is None,"native TLS server exited"
                try:
                    with socket.create_connection(("127.0.0.1",port),timeout=.1):
                        break
                except OSError:
                    assert time.monotonic()<deadline,"native TLS server did not bind"
                    time.sleep(.025)

            def tls_rpc(request,identity):
                context=ssl.create_default_context(cafile=native_certificate["CA certificate"])
                context.load_cert_chain(identity["Certificate"],identity["Private key"])
                with socket.create_connection(("127.0.0.1",port),timeout=5) as connection:
                    with context.wrap_socket(connection,server_hostname="127.0.0.1") as secured:
                        assert ("URI",f"urn:firecrab:host:{original['installationId']}") in secured.getpeercert()["subjectAltName"]
                        data=json.dumps({"version":1,"request":request}).encode()
                        secured.sendall(struct.pack("!I",len(data))+data)

                        def exact(count):
                            value=b""
                            while len(value)<count:
                                chunk=secured.recv(count-len(value))
                                if not chunk:
                                    raise EOFError("native channel closed before admission")
                                value+=chunk
                            return value

                        length=struct.unpack("!I",exact(4))[0]
                        assert 0<length<=16384
                        return json.loads(exact(length))

            remote=tls_rpc({"action":"identity"},api_certificate)
            assert remote==identity
            for request,principal in [({"action":"identity"},sibling_certificate),
                                      ({"action":"observe","endpoint_id":str(uuid.uuid4()),"resource":resource},api_certificate)]:
                try:
                    tls_rpc(request,principal)
                except (ssl.SSLError,EOFError,ConnectionError,OSError):
                    pass
                else:
                    raise AssertionError("native TLS accepted the wrong API principal")
        finally:
            server.terminate()
            server.wait(timeout=5)
        print(json.dumps({"kind": "privileged-native-maintenance-fixture", "passed": True,
                          "nativePlatform": original["platform"], "originalOperation": operation,
                          "publicServiceControlGuarded": True,
                          "hostConfigurationOwnership":True,"standaloneInstallerFenced":True,
                          "installerParentScopeChecked":True,
                          "nativeMtls":True,"wrongApiPrincipalRejected":True,
                          "liveSystemd": False, "guestBoot": False,
                          "stackExecution": False, "terraformExecution": False}))


if __name__ == "__main__":
    main()
