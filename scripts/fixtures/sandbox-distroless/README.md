# Prepared distroless QA app

This small static app is an opt-in QA payload. It is not a guest agent. Its
default entrypoint waits without requiring a distribution init system. It does
not install packages, open a listener, generate keys, or contact a provider.

The public source image remains separate from the operator-prepared image.
The prepared image adds BusyBox shell tools, key-only SSH dependencies and the
static guest helper through `packaging/sandbox/distroless.Dockerfile`. It has no
package manager, OpenRC or systemd. Normal VM specialization generates the
unique operator and host keys; none belong in the build context.
The preparation changes only root's login shell to the supplied `/bin/sh`.
Other passwd entries, the application user and password fields remain unchanged.
The SSH service continues to disable password authentication. A distroless root
entry pointing to absent `/sbin/nologin` cannot authenticate even with a valid key.

## Build and record

Use an owned output directory. Compile `probe.c` on the target Linux
architecture with `cc -std=c11 -Os -static -Wall -Wextra -Werror`. Record the
compiler image digest, installed build-package versions and binary SHA256.
The app image Dockerfile expects that binary as `probe` in its private context.
Set `DISTROLESS_IMAGE` to an architecture-specific public manifest digest.

Build the app image, then use its immutable local image ID as `APP_IMAGE` in
the existing distroless preparation Dockerfile. Supply a digest-pinned Alpine
`SSH_TOOLS_IMAGE` and the verified static `firecrab-guest-console` binary. Do
not copy the Alpine root filesystem or package database into the final image.
The preparation checks the dependency listing and executes the isolated SSH
loader before completing. Package-resolution or loader errors must fail build.

For limited local QA, the legacy builder accepts `--memory 512m --memory-swap
512m --cpu-period 100000 --cpu-quota 100000 --force-rm` with
`DOCKER_BUILDKIT=0`. Apply the same CPU/memory bounds to any compiler or smoke
container. Never prune shared images or caches.

Save exactly the final prepared tag with `docker save`. Record its image ID,
archive SHA256 and byte count. `scripts/serve-sandbox-oci-archive.py` constructs
a content-addressed manifest from that one archive. Its manifest digest is
distinct from the Docker image ID and archive checksum. Use the manifest digest
for the owned loopback registry reference. Preserve the original source pin,
preparation Dockerfile, helper, app and registry-script checksums alongside it.

For a key-only SSH image smoke, export the prepared rootfs from an owned stopped
container. Run `scripts/test-sandbox-distroless-ssh.sh ROOTFS_ARCHIVE
EPHEMERAL_KEYS OUTPUT_DIRECTORY` inside an owned Linux Docker container with
`FIRECRAB_DISTROLESS_DOCKER_SMOKE=1`, CPU 1, memory 512 MiB and `--network none`.
Use the pinned preparation tools stage, which contains the SSH client. Mount the
rootfs archive and keys read-only. Provide newly generated `client`,
`client.pub`, `ssh_host_ed25519_key` and a `known_hosts` entry pinned to
`[127.0.0.1]:2222`. Never use an account or existing VM key.

The script extracts only inside its disposable container and supplies runtime-only
server keys, then uses the container's loopback. It requires the prepared root
shell without changing it, so a missing-shell preparation error fails the smoke.
Preserve server logs proving `sshd-auth`, `sshd-session` and accepted public-key
authentication, plus actual command output and exit 7. Its report explicitly
sets MicroVM and V1-channel validation to false. Remove these disposable private
keys after the smoke; retain public fingerprints and evidence hashes.

## Actual guest checks

Image-build and container smoke results do not validate a MicroVM or an
authenticated channel. Import and create resources only after operator approval.
Create fresh owned environment resources. Keep existing VMs, environments,
connections and shared images unchanged.

Record actual `/proc/1/comm`, `/proc/1/cmdline`, `/etc/os-release` and the image's
application PID. Check package-manager, OpenRC and systemd paths explicitly.
The real `guest/health` endpoint must return version 1, `init: distroless` and
`execution: true` through the admitted runtime and pinned SSH channel.
An unprepared image's unsupported response does not pass this check.

Use one immutable task ID and exactly one `run_argv` invocation per case. Do not
resubmit an uncertain command. Inspect/reconcile that identity instead. Retain
the typed input and actual command event, including separate stream bytes,
exit status, task/attempt/runtime/environment references and limits. A completed
assistant reply alone is not evidence. Native tool environment values use the
`[{"name": "QA_NONCE", "value": "..."}]` shape; the REST V1 DTO uses a map.

| Probe argv after `/usr/local/bin/firecrab-qa-probe` | Requested limits | Required evidence |
| --- | --- | --- |
| `--report`, then a literal argument containing shell syntax | wall 10, CPU 3, memory 64 MiB, output 8192 | Relative cwd under the owned workspace; exact argument, explicit environment and stdin hex; nonzero UID/GID; zero supplementary groups; `NoNewPrivs=1`; soft/hard nofile 1024, address space 64 MiB, CPU 3; separate `QA_STDERR:NONCE`; exit 7 |
| `--memory` | wall 10, CPU 3, memory 32 MiB, output 2048 | Allocation refused with ENOMEM, exit 42 |
| `--cpu` | wall 10, CPU 1, memory 64 MiB, output 2048 | Actual CPU termination before the wall deadline; no owned process remains |
| `--overflow` | wall 10, CPU 3, memory 64 MiB, output 16 | Exit 125, at most 16 bytes per stream; no owned process remains |
| `--sleep` | wall 2, CPU 3, memory 64 MiB, output 2048 | Ready marker, measured bounded duration and native hard-KILL exit 137 on BusyBox; no owned process remains |
| `--sleep` | wall 120, CPU 3, memory 64 MiB, output 2048 | Inspect the live exact boot/PID/start tuple; reject a modified tuple; cancel through the task API and verify no owned process remains |

For `--report`, set both `QA_NONCE` and `QA_LITERAL`. Provide stdin, including a
literal newline or NUL if testing that bound. Every byte-valued field is hex to
avoid ambiguous escaping. Parse actual values instead of matching prose.
Exit 7 and 42 are intentional probe outcomes; do not record them as successful
exit-zero tests through `record_test`.
Preserve native signal exits. The actual BusyBox deadline returned 137 and an
explicitly cancelled task retained 143. Do not convert these values to 124 or
classify a termination from the number alone. Use the requested bounds, measured
elapsed time, explicit cancellation action and exact process identity together.
Observe live processes through the scoped API and `/proc` command identity.
A bounded stdout reader may not flush a short ready line until the command ends.

Prove static UID/timed-lock fallback independently. Confirm that `setpriv` is
absent and any injected BusyBox `flock` lacks timed `-w`, then run the shipped
helper probe and inspect actual command identity. A lock contender must fail
within its requested bound while the inherited supervisor lock remains held.
Never remove tools from a running retained Workspace to force fallback.

Use two newly owned VMs for SSH key isolation. Pin each destination's real host
key and disable agent, user config, forwarding, passwords and interactive auth.
Each correct VM key must authenticate only to its own VM. The first key must
fail public-key authentication on the second VM. Preserve that refusal and
verify the second VM's authorized-key hash did not change. Do not borrow an
unrelated VM or key for this check.

Export evidence before cleanup. Retain failed/uncertain task records and exact
resource IDs. Delete only the explicitly approved resources after result and
artifact export. Workspace deletion needs its separate explicit authorization.
