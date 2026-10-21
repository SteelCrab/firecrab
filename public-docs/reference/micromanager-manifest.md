# microManager installation and runtime TOML

[packaging/micromanager.toml](../../packaging/micromanager.toml) defines Debian, Firecrab, Firecracker, kernels, platform images, URLs, and checksums.
The CLI embeds it at build time.
Maintainers change the manifest and build a new CLI to publish different installation inputs.
macOS uses ARM64 assets; Windows selects x86_64 or ARM64.

## Runtime settings

Edit the existing `micromanager.toml` in the management directory:

| Host | Default installed TOML |
| --- | --- |
| macOS | `~/Library/Application Support/Firecrab/micromanager/micromanager.toml` |
| Windows | `%LOCALAPPDATA%\Firecrab\micromanager\micromanager.toml` |

`FIRECRAB_MICROMANAGER_HOME` overrides the management directory. Linux runs the
host services directly and has no MicroManager runtime TOML.

Add or edit these sections without changing the release fields:

```toml
[runtime]
auto_restart = true

[runtime.ssh]
connect_timeout_secs = 10
keepalive_interval_secs = 15
keepalive_count_max = 3
reconnect_attempts = 5
reconnect_delay_secs = 5
restart_vm_on_failure = true
reuse_connections = true
persist_secs = 60
```

These are the defaults. An older TOML without the sections uses the same defaults.
A missing installed TOML remains a legacy installation; normal commands do not
create a new release record or migrate its OS. Use an explicit installation to
create the full installed record before adding runtime sections.

- `auto_restart` controls macOS launchd `KeepAlive` and Windows' one-minute
  scheduled-task trigger. `false` disables automatic relaunch; explicit starts
  and the normal login startup remain available. Explicit `service stop` still
  unloads/disables the host job regardless of this setting.
- The SSH settings apply to macOS. Windows keeps using WSL command execution
  and localhost forwarding, without adding an SSH dependency.
- `connect_timeout_secs` accepts 1–300. Keepalive sends a check after
  `keepalive_interval_secs` of inactivity (0–3600; 0 disables checks) and uses
  `keepalive_count_max` unanswered checks (1–100) to detect a dead connection.
- `reconnect_attempts` accepts 0–1000 and `reconnect_delay_secs` accepts 1–300.
  Both startup and an established API tunnel retry failed SSH connections.
  With `restart_vm_on_failure = true`, exhausted retries stop the management VM;
  `auto_restart` determines whether the host supervisor relaunches it.
  Set `restart_vm_on_failure = false` to retain the VM and continue reconnecting
  after the retry threshold. This avoids stopping nested workloads for a tunnel
  outage. It does not repair a crashed kernel or a damaged filesystem.
- `reuse_connections` shares SSH transport between the resident API tunnel,
  development commands, shell, and diagnostics. TCP forwards use dedicated
  transports so stopping a relay releases its port. Disabling reuse also
  overrides connection sharing from the user's SSH configuration. The resident
  tunnel remains a foreground process owned by the supervisor. A command-created
  connection can stay idle for `persist_secs` (0–86400; 0 means indefinitely).
  Private control sockets use a short host-side directory outside the VM share.

Run `firecrab service start` after editing to regenerate the host registration.
On macOS this restarts the management VM and interrupts nested workloads.
On Windows it updates the task and resumes its holder without terminating WSL.
Changes do not reload a live tunnel in place. New standalone SSH
commands read the file immediately; a reused connection keeps the transport
policy selected when that connection opened. The SSH timeout does not change the
CLI's VM/API startup readiness deadline.

Runtime edits do not change the installation fingerprint or require OS
reprovisioning. `install` and `reinstall` retain the current runtime settings while
selecting release inputs from the new CLI. Invalid settings fail before stopping
the service. Preserve the rest of the installed TOML when editing.

## Command behavior

| Command | Manifest behavior |
| --- | --- |
| First `service install` | Install from the bundled TOML |
| `service install` on a legacy or changed installation | Migrate to the bundled TOML |
| `service install` with a completed matching manifest | Reuse the installation |
| `service reinstall` | Clear registration, old scripts, installed TOML, and completion records; provision again |
| `start`, `stop`, `status`, `dev`, `repair` | Use existing assets and provisioning state |
| `service uninstall` | Remove registration; retain managed data |
| `service uninstall --purge` | Also remove disks, keys, downloads, scripts, and TOML; Windows unregisters Debian |

Updating the CLI alone does not migrate existing installations.
`service dev` installs only when the management VM is missing.
An installation without `<managed-home>/micromanager.toml` uses legacy pins for validation and offline repair.

## Install and migrate

```sh
firecrab service install
# Force reprovisioning when needed:
firecrab service reinstall
```

Both commands validate the bundled TOML and artifact checksums.
The guest reports its manifest fingerprint after provisioning and the nested workload check.
The CLI saves `<managed-home>/micromanager.toml` only after the resident service and localhost API are healthy.
Failures do not publish a new installed manifest.
The release sections record installation inputs for validation and repair.
The next explicit installation selects the new CLI’s bundled release inputs and retains the editable runtime settings.

### macOS

Migration replaces the Debian OS and boot files and preserves the data disk and management SSH client keys.
It clears the cached SSH server fingerprint because the new OS creates new server keys.
A changed manifest reprovisions the OS; `reinstall` always rebuilds it.
The selected Debian kernel and matching initrd become `system/Image` and `system/initrd.img`.
Installation checks both nested Firecracker and resident boot with that kernel before saving the manifest.

### Windows

Migration reprovisions the existing WSL distribution without unregistering or reimporting it.
The rootfs archive is used only for a missing distribution.
Reinstall clears task registration, scripts, and completion records first.
TOML provisioning removes Firecrab development overrides so installed release binaries start.
User overrides and distribution data remain.
The selected Debian kernel boots the nested test; Microsoft's shared WSL kernel stays unchanged.
Other WSL distributions stay unchanged.

## Change release inputs

Update versions, HTTPS URLs, upstream checksums, and each architecture's Debian 13 kernel together.
Unsupported schemas, unknown fields, unsafe filenames, and malformed checksums fail before downloading or stopping.
An unavailable pinned kernel fails provisioning; no alternative kernel is selected.
Publish an updated manifest when mirrors no longer provide the package.

## Related

- [macOS setup](../getting-started/macos.md)
- [Windows setup](../getting-started/windows.md)
- [Source development](../development/micromanager.md)
