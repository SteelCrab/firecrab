# microManager development

Build the CLI on the host and the Linux API/helper inside managed Debian.
Install Node.js and npm on the host for the development dashboard.
Run commands from the checkout root with the pinned Rust toolchain.
See [Contributing](../../CONTRIBUTING.md#run-from-source) for native Linux development.

## macOS

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service dev
./target/debug/firecrab service debug --logs --tail 100
curl -fsS http://127.0.0.1:5523/api/host
```

The helper build script applies the Virtualization entitlement and ad-hoc signature.
The checkout CLI finds its signed sibling helper.
`FIRECRAB_MICROMANAGER_HELPER` can select another helper explicitly.

To replace installed CLI/helper binaries, use `./target/debug/firecrab service reinstall`.
This reprovisions the OS and preserves managed data.
See [macOS setup](../getting-started/macos.md) for host requirements.

## Windows

Run in PowerShell:

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service dev
.\target\debug\firecrab.exe service debug --logs --tail 100
Invoke-RestMethod http://127.0.0.1:5523/api/host
```

The CLI uses `tar.exe` and `wsl.exe` to transfer the snapshot and build script.
It normalizes CRLF shell scripts and builds for the Debian architecture.
Continue using the checkout executable to test CLI edits.
Windows `service reinstall` does not copy it into the user's installed binary path.
See [Windows setup and release limits](../getting-started/windows.md).

## Build and deploy

`service dev` installs microManager if missing, then starts its resident service.
It uploads a source snapshot, installs compiler prerequisites on the first run, and builds `firecrab-api` and `firecrab-helper` with `--locked`.
Sources and compiler caches persist under `/var/lib/firecrab/dev`.
Local configuration, `.env` files, nested checkouts, and build output are excluded.

After a successful build, deployment switches the two systemd executable overrides and checks guest and host API readiness.
It starts the source dashboard in the background at `http://localhost:8080`.
The command returns only after the dashboard and its API proxy answer successfully.
A build failure leaves services running; an ordinary deployment failure restores previous executables.
Before uploading, starting, or deploying, macOS checks the guest's writable OS/data mounts and critical OS executables.
The shared macOS/WSL deployment script repeats these checks after compilation and before reporting success.
An HTTP response from `/api/host` alone does not establish that the guest filesystem is healthy.
If the filesystem becomes unhealthy, deployment stops and preserves its archive and rollback files for recovery.
Inspect `service debug --logs`, then run `service repair` before retrying.
`service dev stop` skips the filesystem health check so services can still be stopped.
Run `service dev` again after each edit; it does not watch files.
Guest service restarts interrupt API/helper requests, so stop workloads before changing their runtime.

| Command or option | Effect |
| --- | --- |
| `service dev --source PATH` | Select another checkout |
| `service dev --release` | Optimize the source build |
| `service dev --yes` | Skip pending-download confirmation |
| `service dev start` / `stop` / `restart` | Control guest API/helper and the development dashboard without rebuilding |
| `service dev --restore` | Remove development overrides and restart installed release binaries |
| `service shell` | Open a root shell in managed Debian |

`dev start`, `stop`, and `restart` require a running management VM.
They preserve deployed binaries and overrides and cannot be combined with build/restore options.
`--restore` needs no checkout and cannot be combined with `--source`, `--release`, or `--yes`.
Overrides persist across `service stop` and `start` until restored.
`service start` resumes selected binaries; it does not build edited sources.

## Frontend

`service dev` installs locked frontend dependencies with `npm ci` when needed.
It launches the selected checkout's Vite server on `http://localhost:8080`.
The server continues after the command exits. Repeated commands reuse a healthy server.
Selecting another checkout replaces only the dashboard started by Firecrab.
An unrelated server on port 8080 causes an error; Firecrab does not stop it or select another port.
Stop a manually started Vite server before using `service dev`.

`dev start` and `dev restart` use the last selected checkout, even from another directory.
`dev stop`, `service stop`, and `service dev --restore` stop the development dashboard.
Dashboard logs are stored at `runtime/dev-dashboard.log` in the managed home.
The installed release dashboard remains available through the API's port 5523.
Vite proxies `/api` and `/ws` to `127.0.0.1:5523` through the macOS SSH tunnel or WSL localhost forwarding.
Use `localhost` for the configured browser origin.
`service dev` and `service dev start` print the API URL and the separate frontend command.
They do not start Vite; port 8080 is available only while the frontend command runs.

## macOS disk I/O

The signed helper attaches writable Linux disks with host caching enabled and full synchronization for guest flush requests.
This applies to provisioning, normal boots, and offline OS repair.
It follows the filesystem corruption workaround adopted by [Lima](https://github.com/lima-vm/lima/pull/2026) and [UTM](https://github.com/utmapp/UTM/pull/5919).
Matching symptoms do not establish that every guest I/O error has the same cause.
After updating the helper, stop and start the management VM to apply the attachment policy.
Repair an already damaged OS filesystem before resuming development.

Run the deployment transaction regression tests without a VM:

```sh
python3 scripts/test-micromanager-dev.py
```

On a healthy installed macOS VM, verify that read-only OS, data, and executable mounts reject both start and build operations:

```sh
python3 scripts/ci-qa-macos-dev.py --cli ./target/debug/firecrab
```

The QA script injects failures in private mount namespaces.
It requires the host mounts, API response, and running service PIDs to remain unchanged.

## Related

- [Architecture](../reference/architecture.md)
- [CI and runtime E2E](ci.md)
- [Repair QA](macos-repair.md)
