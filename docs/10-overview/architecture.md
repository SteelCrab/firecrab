# Architecture

firecrab is a self-hosted microVM manager for one Linux host.
It uses Firecracker for virtual machines and a browser dashboard for daily work.

It is not a hosted service.
It is not a multi-host scheduler.

## Components

```text
Browser dashboard or REST client
             |
             | HTTP and WebSocket
             v
firecrab-api
  |          |             |
  |          |             +-- SQLite state
  |          +---------------- Firecracker process per VM
  +--------------------------- Unix socket
             |
             v
firecrab-net-helper
  +-- bridge, TAP, nftables, and dnsmasq
```

| Component | Role |
| --- | --- |
| `firecrab-api` | REST API, WebSocket console, state, and VM processes |
| `firecrab-net-helper` | Small privileged network service |
| `firecrab-helper-protocol` | Messages shared by the API and helper |
| `firecrab-api-types` | API request and response types |
| `firecrab-frontend` | React and TypeScript dashboard |

## Privilege boundary

The API runs without host network privileges.
It sends a small set of typed requests to the network helper.

The helper has the capability needed to change host networking.
It derives interface names from UUIDs instead of accepting shell-ready names.

This boundary limits the effect of an API bug.
See the [network helper guide](../20-guides/net-helper.md).

## State

| State | Default location |
| --- | --- |
| VM and network records | `data/firecrab.db` |
| VM disks and runtime files | `data/vms/<vm-id>/` |
| Kernels and root filesystems | `images/` |
| Bridge, TAP, and firewall state | Linux kernel and runtime services |
| Start progress | API memory |

SQLite uses WAL mode.
Installed deployments use `/var/lib/firecrab` as the working directory.

## VM start flow

1. The API changes the VM state to `starting`.
2. It prepares or reuses the writable root filesystem.
3. It asks the helper to prepare the MicroNetwork and TAP device.
4. It writes a Firecracker configuration and starts one process.
5. It waits for the guest to report `FIRECRAB_NETWORK_READY`.
6. It changes the VM state to `running`.
7. A monitor updates state and removes runtime network resources after exit.

The guest readiness message matters.
A host-side DHCP lease alone does not prove that the guest network works.

## Security notes

The default HTTP listener is `127.0.0.1:3000`.
Do not expose an unprotected listener to another network.

Image files are checked before use.
Template paths are restricted to the configured image root.

## Related documents

- [Glossary](glossary.md)
- [AWS comparison](aws-mapping.md)
- [REST API](../20-guides/api.md)
- [MicroNetwork](../20-guides/explicit-micro-network.md)
- [MicroStorage](../20-guides/micro-storage.md)
