# Guest runtime

Firecrab injects guest services during image import and specializes VM disks at start.
Host `/etc/firecrab/api.env` configures the API; guest `/etc/firecrab` contains runtime files.

## Init and boot

OCI imports preserve a supported systemd or OpenRC installation.
Other images use BusyBox init.
The decision is recorded in `/etc/firecrab/init-system`.
Older imported templates retain their previous behavior until re-imported.
See [OCI activation](oci-import.md#guest-activation) for detection rules.

| Guest path | Purpose |
| --- | --- |
| `/etc/firecrab/busybox` | Static utilities; PID 1 only for the fallback init path |
| `/etc/firecrab/rc.boot` | Fallback mounts, metrics, DHCP, readiness, and service startup |
| `/etc/firecrab/rc.console`, `/etc/firecrab/rc.serial` | Fallback / native-init serial console wrappers |
| `/etc/firecrab/dhcp.script` | Apply DHCP address, route, and DNS |
| `/etc/firecrab/services.d/app` | OCI Entrypoint, Cmd, Env, and WorkingDir as a service |
| `/etc/firecrab/services.d/sshd` | Key-only root SSH service |
| `/etc/firecrab/base-packages.ok` | Successful first-boot package-install marker |
| `/usr/local/sbin/firecrab-guest-agent` | CPU and memory sampling |

Native init owns mounts and service supervision.
BusyBox uses `/etc/inittab` to run boot setup once and respawn the console.
Both paths publish `FIRECRAB_NETWORK_READY` after network setup.
The image's app runs after readiness and is never PID 1.

Inspect the running init from inside the guest:

```sh
readlink -f /proc/1/exe
tr '\0' ' ' < /proc/1/cmdline; echo
cat /etc/firecrab/init-system
```

`/sbin/init` alone does not prove which executable is running.

## Environment

VM `env` is stored in `/etc/firecrab/vm.env` in plaintext.
For images with `services.d/app`, VM startup inserts this block after image exports and before `exec`:

```sh
# >>> firecrab vm env
. /etc/firecrab/vm.env
# <<< firecrab vm env
```

VM values override image values.
An empty map removes the source block.
A running VM environment update rewrites the file and restarts the app service.
Images without that service ignore runtime environment changes; check `hasGuestService` on the image record.

## Shell revisions

Selecting `shellIds` pins each shell's latest revision to the VM.
At start, the API injects those revisions into `/var/lib/firecrab/shells`.
A boot oneshot runs them after guest networking is ready.
Catalog Alpine uses OpenRC; Ubuntu/Rocky use systemd.
Prefer POSIX `/bin/sh` scripts.

## Metrics

The guest agent emits `FIRECRAB_USAGE` lines on the serial console.
Memory used is `MemTotal - MemAvailable`.
The API parses those samples and removes them from interactive terminals and console logs.
Without an agent, usage is null and startup still succeeds.
Stop/start refreshes the injected agent after an API upgrade.

## SSH

VM creation generates an operator ed25519 key pair under the VM's host-side `ssh/` directory.
At start, the public key is injected into `/root/.ssh/authorized_keys` and per-VM host keys into `/etc/ssh/`.
The dashboard downloads the private key and displays the guest host-key fingerprint.
Check that fingerprint before connecting.
See the [dashboard SSH procedure](../guides/dashboard.md#console-and-ssh) and [API endpoints](api.md#vm-endpoints).

Missing `sshd` can reflect incomplete first-boot package installation.
Keep the guest console log and distinguish network readiness from SSH readiness.

## Related

- [OCI import pipeline](oci-import.md)
- [API VM fields](api.md#vm-fields)
- [Storage paths](../guides/storage.md#installed-microvm-state)
