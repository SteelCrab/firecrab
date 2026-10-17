# Dashboard

Open `http://127.0.0.1:5523/` after installation.

## VM workflow

1. Create a MicroNetwork on **Networks**.
2. Install a template or import an OCI image on **Images**.
3. Open **MicroVM → Create** (`#/vms/new`).
4. Select the image and network; set CPU, RAM, disk, storage, and egress.
5. Create the VM, then start it from the list.
6. Open **Terminal** after its state becomes `running`.

Row actions are in the `⋯` menu.
CPU, RAM, disk, and egress edits require an inactive VM.
Disks can grow only.
Environment edits can run while the VM is running; saving restarts its app service.
Values are stored in plaintext and require an image with a guest app service.

## Console and SSH

The terminal uses the guest serial console.
`exit` ends the login session and leaves the VM running.
Select **New session** to attach again; **Log** retains previous output.

Open the SSH panel from the terminal, VM details, or `⋯ → SSH connect`.

1. Download `firecrab-<name>.pem` and restrict its permissions with `chmod 600`.
2. Use **check ipv4** or **check ipv6** to compare the guest's host key.
3. Require `MATCH`, then use the displayed `ssh -i` command.

The fingerprint identifies the guest host key.
It must also match the first SSH connection prompt.
For remote access, use the panel's proxy-jump command or create a TCP port forward.
Replace `<hostIP>` with a reachable Firecrab host address.
An external client may also need router forwarding.

## State and usage

The list shows one **VM** label for lifecycle state.
Green means running, amber means starting or stopping, red means error, and gray means inactive.
Hover or focus the label for the VM's name, ID, state, and description.
Tooltips fit the viewport, stay open while hovered, and close with Escape.

VM details show **API-STATUS** when the API recorded a startup or explicit network-recovery result.
The section identifies Firecrab API (`firecrab-api`) and includes the outcome, information,
diagnostics, and check time in English or Korean.
Check times use `YYYY-MM-DD HH:mm:ss` with the viewer's UTC offset.
Results are snapshots; a new VM start clears the previous result.
VMs without a recorded result have no **API-STATUS** section.
See the [reconciliation contract](../reference/api.md#api-startup-reconciliation) for each outcome.

Guest CPU and memory require the Firecrab Metrics Agent.
An image without the agent can still start; its metrics remain unavailable.
Stop and start a VM after an API upgrade to refresh its guest agent.

## Resource screens

| Screen | Main actions |
| --- | --- |
| Networks | Create IPv4-only or dual-stack networks; choose SLAAC or DHCPv6 for IPv6 |
| Storage | Register mounted host directories for VM disks |
| Images | Install M2Images, inspect/import OCI, and register custom images locally |
| Kernels | Install verified kernels, pair them with unused images, and delete unused kernels |
| Host | Inspect OS, capacity, build information, and API/helper service states |
| Pipe | Edit domain/path routes and app addresses; apply or stop [Nginx ingress and an optional tunnel](pipe.md) |
| Update | Read release notes, then follow the gauge, percent, and stage until the new API answers |

MicroBoot image bootstrap is [API-only](images.md#bootstrap).
The Host panel refreshes service states every ten seconds and clears stale states when the API is unavailable.

## Related

- [Networking](networking.md), [Storage](storage.md), and [Images](images.md)
- [API](../reference/api.md) and [CLI](../reference/cli.md)
- [Development setup](../../CONTRIBUTING.md#run-from-source)
- [Troubleshooting](troubleshooting.md)
