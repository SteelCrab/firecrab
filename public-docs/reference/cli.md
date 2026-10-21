# CLI

`firecrab` manages the same REST API as the dashboard on Linux, macOS, and Windows.
Install it with the [CLI installer](../getting-started/installation.md#cli-only-installation).
Use `firecrab --help` or `firecrab COMMAND --help` for flags.

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

## Resource commands

| Resource | Commands |
| --- | --- |
| VM | `vm list`, `create`, `start ID`, `stop ID`, `delete ID` |
| MicroService | `microservice list`, `get ID`, `create --file JSON`, `update ID --file JSON`, `delete ID`, `deploy ID --host-port PORT`, `status ID`, `stop ID`; see [MicroService](../guides/microservices.md) |
| Console | `vm console ID` (`terminal` alias) |
| Network | `network list`, `create`, `delete ID` |
| Image | `image list`, `inspect REFERENCE`, `import REFERENCE`, `import-status ALIAS` |
| Host profile | `host add`, `list`, `use`, `show`, `remove` |

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
```

Replace IDs with values returned by the previous commands.
Defaults are one vCPU, 512 MiB RAM, and 2 GiB disk.
Override with `--cpu`, `--ram`, and `--disk-gb`.
Use `--egress internet|isolated` and `--storage-root ID` for policy and disk placement.

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
