#!/usr/bin/env python3
"""Exercise the production OCI startup worker in a disposable Linux fixture."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def wait_for(predicate, message, console):
    deadline = time.monotonic() + 10
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError((message, console.read_text() if console.exists() else ""))
        time.sleep(.01)


def main():
    source = (ROOT / "firecrab-api/src/oci/provision.rs").read_text()
    start = source.index("# The image carries")
    worker = source[start:source.index('\n"#,', start)]
    ending = ") </dev/null >/dev/console 2>&1 &"
    assert worker.count(ending) == 1, "Identify the complete background package worker"
    worker = worker.replace(ending, ': > "$QA_WORKER_DONE"\n' + ending)
    for preinstalled in (False, True):
        with tempfile.TemporaryDirectory(prefix="firecrab-oci-startup-") as directory:
            root = Path(directory)
            services = root / "services"
            services.mkdir()
            scripts = {
                "app": '#!/bin/sh\n: > "$QA_APP_READY"\n',
                "sshd": '''#!/bin/sh
[ "$QA_PREINSTALLED_SSH" = true ] || [ -f "$QA_PACKAGES_DONE" ] || exit 92
printf '%s\\n' "$$" >> "$QA_SSH_STARTS"
: > "$QA_SSH_READY"
while [ ! -f "$QA_RELEASE_SSH" ]; do sleep .01; done
printf '%s\\n' "$$" >> "$QA_SSH_STOPS"
''',
            }
            for name, text in scripts.items():
                path = services / name
                path.write_text(text)
                path.chmod(0o755)
            binary = root / "sshd-binary"
            if preinstalled:
                binary.write_text("fixture")
                binary.chmod(0o755)
            fastfetch, toolbox = root / "fastfetch", root / "bb"
            for path, text in [(fastfetch, "#!/bin/sh\nexit 0\n"),
                               (toolbox, '''#!/bin/sh
# BusyBox implements kill itself; minimal Linux fixtures need no external kill.
case "$1" in
  kill) shift; kill "$@" ;;
  *) exec "$@" ;;
esac
''')]:
                path.write_text(text)
                path.chmod(0o755)
            packages = '''
for i in $(seq 1 1000); do [ ! -f "$QA_APP_READY" ] || break; sleep .01; done
[ -f "$QA_APP_READY" ] || exit 90
if [ "$QA_PREINSTALLED_SSH" = true ]; then
  for i in $(seq 1 1000); do [ ! -f "$QA_SSH_READY" ] || break; sleep .01; done
  [ -f "$QA_SSH_READY" ] || exit 91
  : > "$QA_PACKAGE_SAW_SSH"
else
  [ ! -f "$QA_SSH_READY" ] || exit 91
fi
: > "$QA_PACKAGES_DONE"
'''
            text = worker.replace("{{", "{").replace("}}", "}")
            replacements = {
                "{services}": str(services), "{base_packages}": packages,
                "{prepare_runtime}": ":", "/usr/sbin/sshd": str(binary),
                "/run/firecrab": str(root / "run"), "/dev/console": str(root / "console"),
                "/usr/bin/fastfetch": str(fastfetch),
            }
            for key, value in replacements.items():
                text = text.replace(key, value)
            env = dict(os.environ, BB=str(toolbox), QA_PREINSTALLED_SSH=str(preinstalled).lower())
            for key, name in [("QA_APP_READY", "app-ready"), ("QA_SSH_READY", "ssh-ready"),
                              ("QA_SSH_STARTS", "ssh-starts"), ("QA_SSH_STOPS", "ssh-stops"),
                              ("QA_PACKAGES_DONE", "packages-done"), ("QA_WORKER_DONE", "worker-done"),
                              ("QA_RELEASE_SSH", "release-ssh"), ("QA_PACKAGE_SAW_SSH", "package-saw-ssh")]:
                env[key] = str(root / name)
            process = subprocess.Popen(["/bin/sh", "-c", text], stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, env=env, start_new_session=True)
            try:
                _, stderr = process.communicate(timeout=10)
                assert process.returncode == 0, stderr
                wait_for(lambda: (root / "worker-done").exists(),
                         "Background package worker did not finish", root / "console")
                assert (root / "ssh-ready").exists(), (root / "console").read_text()
                starts = (root / "ssh-starts").read_text().splitlines()
                assert len(starts) == 1, (starts, (root / "console").read_text())
                assert (root / "package-saw-ssh").exists() == preinstalled
                print("PASS", "preinstalled SSH before packages, one start" if preinstalled
                      else "missing SSH starts after packages", flush=True)
            finally:
                (root / "release-ssh").touch()
                try:
                    wait_for(lambda: not (root / "ssh-starts").exists()
                             or ((root / "ssh-stops").exists()
                                 and sorted((root / "ssh-starts").read_text().splitlines())
                                 == sorted((root / "ssh-stops").read_text().splitlines())),
                             "Fixture SSH processes did not exit", root / "console")
                finally:
                    # The launcher and package worker share only this fixture's session.
                    try:
                        os.killpg(process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    process.wait(timeout=5)
    print("linux_process_fixture; microvmValidated=false")


if __name__ == "__main__":
    main()
