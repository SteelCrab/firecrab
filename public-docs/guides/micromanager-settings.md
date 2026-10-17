# microManager settings and Sleepy

On Windows and macOS, open the settings editor:

```sh
firecrab service settings
```

The screen edits microManager CPU, memory, VM autostart, Sleepy and idle timeout.
Advanced settings show the data directory and edit the local API port. The data
directory is read only; choose it before installation with
`FIRECRAB_MICROMANAGER_HOME`. It does not move existing VM data.

Use Tab/arrow keys to select, Enter to edit numbers, Space to toggle, Ctrl+S to
save, and Escape to exit. An unsaved draft requires explicit discard. Settings
are validated and saved atomically to `<managed-home>/settings.json`; concurrent
editors cannot overwrite each other's changes.

For scripts, `--json` only reads settings and does not start the VM:

```sh
firecrab service settings --json
firecrab service settings --set sleepy=true --set idle_minutes=10
firecrab service start
```

Save reloads Sleepy policy in an already running controller. Run `service start`
after the first save or after changing CPU, memory or port. Activation requires
an idle guest and a current guest API with `/api/micromanager/activity`. For an
older installed release, deploy this checkout first with `service dev --source
<checkout>`. A missing activity endpoint inhibits sleep; it never counts as idle.

Windows resources use the **global WSL2** `.wslconfig` CPU/memory limits. The
editor preserves other sections and creates a backup before changing an
existing file. Limits take effect after the next WSL2 VM restart. Firecrab does
not shut down the user's other distributions. On macOS, resources are passed
to the native helper; build/install the matching updated helper.

The host controller remains running when the management VM sleeps. It listens
on `127.0.0.1` at the configured port (5523 by default). Windows uses a separate
per-user scheduled task; macOS uses a separate LaunchAgent. The guest holder
has no automatic restart triggers in this mode. The internal listener/tunnel
uses port 5524; use the public controller port for normal clients.
CLI commands use the activated controller port by default. `--api`,
`FIRECRAB_API`, and a selected remote host retain their usual precedence.

Sleepy defaults to off with a 10-minute idle timeout. Enabled Sleepy shuts down
the management VM only when no MicroVM is starting/running/stopping, no detached
image/kernel/OCI/registry/bootstrap/update work is active, and no API request,
console or host-shell lease remains. Unknown activity keeps the VM awake.

Mutating API requests and console connections wake automatically, wait for API
readiness, and are forwarded once. Concurrent requests share a wake. The proxy
does not retry a mutation after sending it upstream. A disconnected client is
not forwarded after its cold boot finishes. Ordinary GET/HEAD/OPTIONS and
`service status` do not wake; while asleep, API reads return HTTP 503 instead of
pretending that VM/image lists are empty. The resident status endpoint is
`GET /api/micromanager/status`.

`service stop` persists a manual stop and blocks automatic wake. `service start`
clears it. Running workloads are never suspended into snapshots by Sleepy.
Changing resources/port requires no active work or console connections.

Native Windows/WSL integration check (requires an installed image and idle VM):

```powershell
./scripts/test-micromanager-sleepy.ps1 -CliPath ./firecrab.exe
```

The script retains PASS/FAILED JSON and checks actual WSL shutdown, cold wake,
open-shell and running-MicroVM sleep inhibition, monitoring and manual stop.
It creates and cleans only its own test VM/network and restores the previous
Sleepy/autostart/idle settings even after a failure. It requires a real Windows
host; portable unit tests do not substitute for native VM validation.
