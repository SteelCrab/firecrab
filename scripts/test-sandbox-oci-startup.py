#!/usr/bin/env python3
"""Exercise the production OCI startup worker in a disposable Linux fixture."""
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    source = (ROOT / "firecrab-api/src/oci/provision.rs").read_text()
    start = source.index("# The image carries")
    worker = source[start:source.index('\n"#,', start)]
    for preinstalled in (False, True):
        with tempfile.TemporaryDirectory(prefix="firecrab-oci-startup-") as directory:
            root = Path(directory)
            services = root / "services"
            services.mkdir()
            scripts = {
                "app": '#!/bin/sh\n: > "$QA_APP_READY"\n',
                "sshd": '''#!/bin/sh
[ "$QA_PREINSTALLED_SSH" = true ] || [ -f "$QA_PACKAGES_DONE" ] || exit 92
echo started >> "$QA_SSH_STARTS"
: > "$QA_SSH_READY"
sleep 1
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
                               (toolbox, '#!/bin/sh\nexec "$@"\n')]:
                path.write_text(text)
                path.chmod(0o755)
            packages = '''
for i in $(seq 1 200); do [ ! -f "$QA_APP_READY" ] || break; sleep .01; done
[ -f "$QA_APP_READY" ] || exit 90
if [ "$QA_PREINSTALLED_SSH" = true ]; then
  for i in $(seq 1 200); do [ ! -f "$QA_SSH_READY" ] || break; sleep .01; done
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
                              ("QA_SSH_STARTS", "ssh-starts"), ("QA_PACKAGES_DONE", "packages-done"),
                              ("QA_PACKAGE_SAW_SSH", "package-saw-ssh")]:
                env[key] = str(root / name)
            result = subprocess.run(["/bin/sh", "-c", text], capture_output=True, env=env, timeout=10)
            assert result.returncode == 0, result.stderr
            for _ in range(200):
                if (root / "ssh-ready").exists() and (root / "packages-done").exists():
                    break
                time.sleep(.01)
            assert (root / "ssh-ready").exists(), (root / "console").read_text()
            assert (root / "ssh-starts").read_text() == "started\n"
            assert (root / "package-saw-ssh").exists() == preinstalled
            print("PASS", "preinstalled SSH before packages, one start" if preinstalled
                  else "missing SSH starts after packages", flush=True)
            time.sleep(1.05)
    print("linux_process_fixture; microvmValidated=false")


if __name__ == "__main__":
    main()
