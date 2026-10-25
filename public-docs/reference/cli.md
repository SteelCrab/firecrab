# CLI

For authenticated hosts, drain, and durable VM placement, use
[`firecrab cluster`](../guides/multihost.md#cli-and-dashboard). Existing
`firecrab host` commands continue to manage saved API profiles.

`firecrab` manages the same REST API as the dashboard on Linux, macOS, and Windows.
Install it with the [CLI installer](../getting-started/installation.md#cli-only-installation).
Use `firecrab --help` or `firecrab COMMAND --help` for flags.

## Schedules

`firecrab schedule` creates and manages time-based MicroVM start and stop operations.
Use `create`, `list`, `get`, `update`, `enable`, `disable`, `runs`, and `delete`.
See [Schedules](../guides/schedules.md) for examples, time zones, and recovery behavior.

## Host profiles

Save a host, then select it by name.
For a remote Linux host, forward its loopback API through SSH first:

```sh
ssh -fN -L 15523:127.0.0.1:5523 user@firecrab-host
firecrab host add dev http://127.0.0.1:15523 --use
firecrab host list
firecrab host show
firecrab --host dev vm list
firecrab host use dev
firecrab host remove dev
```

The first added host becomes current automatically.
Profiles are written atomically to `~/.firecrab/config.toml`.
On Windows, `~` means `%USERPROFILE%`; `FIRECRAB_CONFIG_DIR` overrides the directory.

```toml
current_host = "dev"

[hosts.dev]
url = "http://127.0.0.1:15523"
```

Endpoint selection, in priority order:

1. `--api URL`
2. `FIRECRAB_API`
3. `--host NAME`
4. `current_host` in the config
5. `http://127.0.0.1:5523`

Global flags work before or after a subcommand.
`host show [NAME]` also probes `/api/host`.
Names allow letters, digits, `.`, `_`, and `-`.
URLs require HTTP or HTTPS and reject credentials, queries, and fragments.
The config stores only endpoints; unknown TOML keys block profile changes.

## Private CA and mutual TLS

`firecrab ca init [--dir PATH]` creates a private root CA offline.
`firecrab ca issue-host-cert HOST_UUID [--dir PATH] [--san DNS_OR_IP] [--days 365]`
issues a certificate for an API host or authenticated caller. Repeat `--san` for
additional server names. Existing CA and host output directories are never replaced.
The default directory is `~/.firecrab/pki`, or `FIRECRAB_CONFIG_DIR/pki`.

For HTTPS API and WSS console connections, set `FIRECRAB_TLS_CERT`,
`FIRECRAB_TLS_KEY`, and `FIRECRAB_TLS_CA` to the caller's certificate, private key,
and private CA certificate. Unix files must have mode `0600`.
See [mutual TLS setup](../guides/mtls.md) for issuance, server configuration,
client examples, and certificate renewal. Linux `firecrab doctor` checks the
configured leaf and CA and fails when either expires within 30 days.

## Resource commands

`firecrab stack validate --file stack.yaml [--json]` checks a declarative YAML stack offline.
It does not read host profiles or contact the API, even when `--api` or `--host` is set.
Exit `0` means the declared schema and local constraints passed; failures exit `1`.
With `--json`, both outcomes produce a JSON report on stdout.
See [Stack validation](../guides/stacks.md) for the experimental schema and its limits.
`stack register --name NAME`, `list`, `status NAME`, `bind`, and `unbind` manage host-local ownership metadata.
`stack delete NAME --id STACK_UUID` removes an empty registration after matching its exact UUID; bound resources and disks are never deleted.
`stack plan --file YAML [--json]` compares a manifest to explicitly bound resource UUIDs without applying it.
`stack apply --file YAML [--expected-plan PLAN_JSON] [--operation-id UUID] [--allow-stop] [--no-wait] [--timeout SECONDS] [--json]` executes supported changes with durable status.
`stack apply-status NAME --id OPERATION_UUID` reads redacted progress as JSON.
`stack source configure NAME --file SOURCE_JSON --expected-generation N` configures a GitHub source separately from its resource manifest.
`stack source status NAME`, `history NAME`, and `check NAME --generation N` expose read-only exact-commit comparisons.
`stack source sync NAME --generation N [--recover-operation-id UUID]` resolves Git and applies supported changes. A failed/interrupted apply requires its operation ID before fresh recovery.
`stack source pause|resume NAME --generation N` controls periodic checks and opt-in automatic synchronization. See [Git source synchronization](../guides/stack-git-source.md).
See [Native YAML stack apply](../guides/stack-apply.md) for stop permissions, retry identity and explicit failure recovery.
These commands use normal host selection; see [Stack ownership and planning](../guides/stack-planning.md) for the full workflow.

| Resource | Commands |
| --- | --- |
| VM | `vm list`, `create`, `start ID`, `stop ID`, `delete ID` |
| MicroService | `microservice list`, `get ID`, `create --file JSON`, `update ID --file JSON`, `delete ID`, `deploy ID [--host-port PORT]`, `status ID`, `stop ID`; `--host-port` may be omitted only for services with a domain; see [MicroService](../guides/microservices.md) |
| Deploy | `microservice rollout ID --file JSON`, `releases ID`, `rollback ID --release UUID`, `retry ID`; rollout and rollback accept `--strategy rolling\|blue_green`, `--operation-id UUID`, `--expected-release UUID`, `--no-wait`, and `--timeout SECONDS`; see [Deploy](../guides/deploy.md) |
| Console | `vm console ID` (`terminal` alias) |
| Network | `network list`, `create`, `delete ID` |
| Image | `image list`, `inspect REFERENCE`, `import REFERENCE`, `import-status ALIAS` |
| Host profile | `host add`, `list`, `use`, `show`, `remove` |
| Pipe AI | `ai login`, `login-status`, `connect`, `list`, `verify`, `get-default`, `set-default`, `respond`, `disconnect`; see [Pipe AI](../guides/pipe-ai.md) |
| Local MCP | `mcp`; serves typed tools over stdio through the selected REST host |

Resource commands call the selected API and share its validation rules.
They do not manage Firecracker or host networking directly.
Use `--json` for machine-readable resource output.
Success exits `0`; connection and API errors exit `1`.
API errors retain the HTTP status, response body, and request ID.

### Create a VM

Install or import an image, then create its network and VM:

```sh
firecrab image list
firecrab network create --name app --subnet-cidr 172.31.20.0/24
firecrab vm create --name app-1 --template alpine-3.24.1 --network NETWORK_ID
firecrab vm start VM_ID
firecrab vm pause VM_ID --json
firecrab vm resume VM_ID --json
```

Replace IDs with values returned by the previous commands.
Defaults are one vCPU, 512 MiB RAM, and 2 GiB disk.
Override with `--cpu`, `--ram`, and `--disk-gb`.
Use `--egress internet|isolated` and `--storage-root ID` for policy and disk placement.
Use repeated `--volume NAME_OR_UUID:/guest/path` options for independent data disks.
Manage them with `firecrab volume list|create|inspect|attach|detach|delete`.
See [Persistent volumes](../guides/volumes.md) for the full workflow.

### Console

```sh
firecrab vm console VM_ID
```

The stream includes up to 256 KiB of backlog, then live serial output.
Ctrl+C reaches the guest; Ctrl+] detaches without stopping it.
`exit` ends the login session and exits the client successfully while the VM keeps running.
Local terminal settings are restored on exit and errors.
The console requires a running VM.

### OCI import

```sh
firecrab image inspect nginx:1.27
firecrab image import nginx:1.27
firecrab image import-status nginx-1.27
```

Inspect confirms architecture and alias.
Import returns after the API accepts the job; poll until `succeeded` or `failed`.
A failed status exits `1` and includes the last nonempty job log line.
See [OCI images](../guides/oci.md) for registry access and import limits.

## Local service commands

`firecrab service` controls native systemd on Linux and managed Debian on macOS/Windows.
Use the [macOS](../getting-started/macos.md#service-commands) or [Windows](../getting-started/windows.md#service-commands) command table for managed services.
Use [service dev](../development/micromanager.md) to deploy source-built API/helper binaries.
`firecrab service dev --port 8098` selects the development dashboard port (default 8080).
Development start/restart controls reuse the saved port; the API port stays unchanged.
Git-linked worktrees use [independent development instances](../development/worktree-dev.md).
Backend API/resource ports are allocated and saved separately from the manual dashboard port.
Linux `service dev` uses a private Docker runtime from both primary and linked checkouts.

On macOS and Windows, lifecycle controls have separate scopes:

| Commands | Scope |
| --- | --- |
| `firecrab service start` / `stop` / `restart` | The management VM and its resident host service |
| `firecrab service dev start` / `stop` / `restart` | The selected checkout's development backend and dashboard |
| `firecrab service dev clean` | Restore that environment's release, remove development files, and clear its dashboard state |

Non-development service controls retain their primary scope, even inside a linked worktree.
General VM/API commands do not automatically select the worktree API.
Pass the development command's printed API URL with `--api URL`.
Docker `dev clean` and `--restore` preserve its named data volume and runtime resources.

## Pipe ingress and tunnel

`pipe` manages Nginx routing and the optional Cloudflare Tunnel connector.
It operates on the local Linux host or the managed macOS/Windows guest.
It does not select a remote API through `--host`.

```sh
firecrab pipe install
firecrab pipe render --config pipe.json
firecrab pipe apply --config pipe.json
firecrab pipe status
firecrab pipe disable
```

On Linux, install/apply/disable require root.
On macOS and Windows, run them without host sudo.
See [Pipe ingress and tunnel](../guides/pipe.md) for Nginx installation, manifest fields, dashboard controls, and reload behavior.

## Linux host diagnostics

These commands inspect the Linux machine where the CLI runs.
They cannot diagnose a remote host selected by `--host`.

```sh
firecrab doctor
firecrab doctor --json
firecrab doctor --digest
firecrab info --json
firecrab status --json
```

`doctor` prints a summary plus failed/skipped checks; passing checks stay silent.
Any failed check returns nonzero.

| Check | Verifies |
| --- | --- |
| `kvm` | Read/write access to `/dev/kvm` |
| `firecracker` | Runtime binary and version |
| `ip_forward` | IPv4 forwarding enabled |
| `nft` | Firecrab nftables table |
| `dnsmasq` | Serving process for every bridge |
| `helper_socket` | API account can reach the helper |
| `ufw` | DHCP, DNS, and forwarding permitted |
| `data_root` | One reachable database |
| `images` | Default template artifacts |
| `image_install_tools` | `tar` and `zstd` |
| `selinux_domain` | Services are outside systemd's `init_t` domain |
| `registry_egress` | Registry access as the API account |
| `reflink` | Image and configured disk roots share a filesystem |

`info` reports `version`, `prefix`, `datadir`, `confdir`, `unitdir`, and `apiBase`.
Path defaults follow the installer and the same-named environment overrides.
`status` reports systemd service states plus `/api/host`; it still prints service states when the API is down.
An unavailable API produces `host: null` with `hostError`.

## Linux updates

```sh
firecrab update --check
firecrab update --check --json
sudo firecrab update --apply
```

With no flag, `update` performs a check without downloads or elevated privilege.
A successful check exits `0` whether or not a newer release exists.
Failed checks exit `1`; JSON retains an `error` and omits `latest`.
JSON includes release notes when available.

Apply stages a SHA-256-verified host bundle under `$DATADIR/updates/<uuid>`.
`--apply` displays each stage with a gauge and overall percent.
The helper rechecks it and replaces binaries/dashboard in the installed layout.
Systemd unit files require an installer rerun; see [Operations](../guides/operations.md#upgrade).

| Variable | Default |
| --- | --- |
| `FIRECRAB_RELEASE_REPO` | `SteelCrab/firecrab` |
| `FIRECRAB_RELEASE_API` | GitHub latest-release endpoint |
| `FIRECRAB_RELEASE_BASE` | GitHub release asset root |
| `FIRECRAB_LIBC` | Build target (`gnu` or `musl`) |

## Related

- [API](api.md)
- [Operations](../guides/operations.md)
- [Build from source](../../CONTRIBUTING.md#run-from-source)

### Pause and resume

`vm pause` keeps guest RAM and execution state in the same Firecracker process.
`vm resume` continues it without a guest reboot. RAM remains allocated while paused.
Stop discards that execution state. Host/VMM shutdown also loses it.
Use [pause/resume operations](../guides/operations.md#pause-and-resume-a-microvm) for limits and recovery.

## Warm pools

| Command | Endpoint |
| --- | --- |
| `firecrab pool list [--json]` | `GET /api/pools` |
| `firecrab pool create --name NAME --template ALIAS --network UUID --min-ready N --max-size N [--lease-ttl SECONDS] [--cpu N] [--ram MIB] [--disk-gb GIB] [--egress internet\|isolated] [--storage-root ID] [--json]` | `POST /api/pools` |
| `firecrab pool show POOL [--json]` | `GET /api/pools/{id}` |
| `firecrab pool update POOL [--min-ready N] [--max-size N] [--lease-ttl SECONDS] [--json]` | `PATCH /api/pools/{id}` |
| `firecrab pool delete POOL [--json]` | `DELETE /api/pools/{id}` |
| `firecrab pool acquire POOL [--idempotency-key KEY] [--json]` | `POST /api/pools/{id}/acquire` |
| `firecrab pool leases POOL [--json]` | `GET /api/pools/{id}/leases` |
| `firecrab pool release POOL LEASE_ID [--json]` | `DELETE /api/pools/{id}/leases/{leaseId}` |

`POOL` is a pool UUID or name. `pool acquire --idempotency-key KEY` makes a retry return the same
lease instead of a second VM; the printed lease names the `firecrab vm console` command for its VM.
See [API: Pools](api.md#pools) for sizing, TTL, and release rules.

Pool acquire always sends an idempotency key. If `--idempotency-key` is omitted, the CLI generates
one. On failure it prints a retry command with that key; use it to recover the same lease.

## AI Sandbox

`firecrab sandbox` uses the shared durable Sandbox REST lifecycle. See the [AI Sandbox guide](../guides/ai-sandbox.md#cli-and-local-stdio-mcp) for model verification, project/task JSON, cancellation, review and selected-file apply. `firecrab mcp` exposes the corresponding `sandbox_*` tools.
