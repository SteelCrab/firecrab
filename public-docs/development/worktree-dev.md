# Worktree development instances

Use `firecrab service dev --port PORT` from a Git-linked worktree to keep its development runtime separate from the primary checkout.
Each worktree has its own backend, data, source snapshot, build caches, dashboard state, and lifecycle lock.
The primary development environment and its running MicroVMs remain separate.
Tracked in #495.

## Select an environment

Use a CLI built with worktree instance support and run commands from the selected checkout.
Install Node.js, npm, wasm-pack 0.14.0, and the `wasm32-unknown-unknown` Rust target for the host dashboard.

| Host and checkout | Development backend |
| --- | --- |
| macOS primary checkout | Existing managed Debian VM |
| macOS linked worktree | Separate managed Debian VM |
| Windows primary checkout | Existing managed WSL distribution |
| Windows linked worktree | Private Docker runtime |
| Linux primary checkout or linked worktree | Private Docker runtime |

On macOS and Windows, automatic isolation selects Git-linked worktrees.
The primary checkout keeps its existing `service dev` behavior.
Changing a branch within one worktree reuses that worktree's environment.
The instance ID is derived from the canonical Git directory, rather than the branch name.
`--source PATH` selects the checkout used for this decision.

For example, run these commands in two existing linked worktrees:

```sh
# In the first worktree
firecrab service dev --port 8098

# In the second worktree
firecrab service dev --port 8099
```

Choose a free dashboard port for each environment.
`--port` is manual; its default remains 8080.
It does not allocate a second dashboard port automatically.
An occupied dashboard port causes an error and does not authorize stopping another server.

## Paths and ports

The primary managed home remains unchanged.
Private instance directories are its siblings:

```text
Firecrab/
  micromanager/                     primary managed home
  dev-instances/
    <id>/
      micromanager/
        dev-instance.json
        runtime/
      bin/
```

The default parent is `~/Library/Application Support/Firecrab` on macOS and `%LOCALAPPDATA%\Firecrab` on Windows.
Linux uses `~/.local/share/Firecrab` for these development instances.
An explicit `FIRECRAB_MICROMANAGER_HOME` selects the primary base used to derive the sibling paths.
Private paths are passed to the selected child command; the CLI does not change the caller's environment.

The first run allocates separate localhost API and resource ports and saves them in `dev-instance.json`.
Subsequent runs retain these backend ports.
The dashboard port is saved separately for `dev start` and `dev restart`.
Changing `--port` changes only the selected dashboard address.
Default primary ports 5523, 5524, and 8080 are excluded from automatic backend allocation.
The registry also excludes backend ports reserved by other instances.
Missing, malformed, or foreign private ownership records cause an error; they do not select primary services.

The dashboard proxies `/api` and `/ws` to its own API port.
Its API proxy preserves `Host` and `Origin` for same-origin validation.
The resource bridge also selects the current environment.
Isolated dashboards restrict browser connections so older frontend code cannot fall back to the primary resource server on port 5524.

## Controls and API clients

Run development controls from the same worktree:

```sh
firecrab service dev stop
firecrab service dev start
firecrab service dev restart
firecrab service dev clean
firecrab service dev --restore
```

These commands use the selected worktree's backend and dashboard state.
Control subcommands cannot be combined with build options such as `--source` or `--port`.
Use a build command to choose a new dashboard port.

On macOS, `dev stop` stops that environment's API/helper and dashboard; its dedicated management VM remains available.
On Docker backends, `dev stop` stops the selected container and dashboard.
`dev clean` and `--restore` restore the installed release inside the selected environment and retain application data.
Docker clean and restore do not delete its container, named volume, or private network.
Clean also removes development files and host dashboard state.

Non-development commands such as `service start`, `service stop`, and `service repair` keep their existing primary scope on macOS and Windows.
On Linux, native service commands keep their production systemd scope.
They do not become worktree controls when the current directory changes.

General API commands also keep their existing endpoint-selection rules.
Use the API URL printed by the development command explicitly:

```sh
firecrab --api http://127.0.0.1:API_PORT vm list
```

Replace `API_PORT` with the assigned backend port.
Running `firecrab vm list` from a worktree does not automatically select its private API.
See [CLI endpoint selection](../reference/cli.md#host-profiles).

## macOS backend

Each linked worktree provisions fresh OS and data disks with separate SSH keys, source caches, logs, and VM identity.
It does not attach the primary environment's disks or share its managed home.
Each instance has a separate launchd label and API tunnel.
Its plist stays in its own runtime directory and is loaded explicitly.
Private instances are not all scheduled to boot at the next login.

The guest API can keep its internal port 5523 because each management VM has its own network.
Host-side API and resource listeners use the assigned instance ports.
The existing [macOS requirements](../getting-started/macos.md) still apply, including the signed helper and nested virtualization.

## Windows and Linux Docker backend

Use a local rootful Linux Docker engine with cgroup v2, usable `/dev/kvm`, and `/dev/net/tun`.
Windows/WSL2 must expose nested KVM to that engine.
A Docker installation alone does not establish these prerequisites.
The runtime also requires a writable private cgroup hierarchy for its internal systemd.

The CLI accepts local Unix sockets, Windows named pipes, and loopback TCP Docker endpoints.
Remote Docker contexts and rootless engines are rejected.
The engine architecture must be x86_64 or ARM64.

Each instance owns a labeled image, container, bridge network, and named data volume.
The runtime uses private network, IPC, and cgroup namespaces.
It receives the KVM and TUN devices and the capabilities needed for its internal services.
It does not use host networking, bind the host cgroup hierarchy, or bind the primary managed home.
Source snapshots are uploaded to the selected container.
The backend does not change global WSL configuration or the primary distribution.
Linux `service dev` also uses this backend from the primary checkout, separately from production systemd services.

Native outer-VM resource controls are unavailable for Docker instances.
The development dashboard does not use the primary native resource endpoint as a fallback.
MicroVMs and application data remain in the instance's named volume across development cleanup and release restoration.

## First run and verification

The first run can download a Debian runtime, a Firecrab release, compiler tools, and locked dependencies.
It then builds the source snapshot and dashboard.
Allow disk space for the OS or Docker image, application data, and compiler caches.
Concurrent management VMs also require separate CPU and RAM allocations.
Stop only the intended development environment when reclaiming its runtime resources.

Use distinct dashboard ports and the printed API URLs when checking two worktrees.
Confirm that stopping one environment preserves the other environment and the primary VM.
Linux CI also runs an opt-in check of two real Docker runtimes:

```sh
cargo test -p firecrab-cli --locked \
  micromanager::dev::container::runtime_tests::private_docker_runtimes_keep_namespaces_data_and_stop_independent \
  -- --ignored --exact --nocapture --test-threads=1
```

This check requires local rootful Docker, KVM, and TUN.
It verifies the production image, systemd, private kernel namespaces, writable cgroups, API relay, separate data, and independent stop/cleanup.
It then installs the checksum-verified release and runs the restore transaction in one runtime.
The real API/helper must become healthy while that runtime retains its data and the other runtime keeps serving its marker API.
This check does not build source services or exercise MicroVM lifecycle.
Cleanup validates ownership labels before removing only the randomly named test resources.
Native live validation with multiple management VMs has not yet been completed.
Unit, proxy, and command-fixture tests do not replace that runtime check.

## Related

- [microManager development](micromanager.md)
- [Contributing](../../CONTRIBUTING.md#run-from-source)
- [CLI](../reference/cli.md)
- [CI and runtime E2E](ci.md)
