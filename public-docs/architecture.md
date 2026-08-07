# Architecture

firecrab is a self-hosted microVM manager.
It runs on one Linux host.

## Components

```text
Browser or REST client
        |
        | HTTP and WebSocket
        v
firecrab-api
   |         |          |
   |         |          +-- SQLite
   |         +------------- Firecracker process per VM
   +----------------------- Unix socket
        |
        v
firecrab-net-helper
   +-- bridge, TAP, nftables, dnsmasq
```

| Component | Job |
| --- | --- |
| `firecrab-api` | API, VM lifecycle, state, and console |
| `firecrab-net-helper` | Privileged host networking |
| `firecrab-frontend` | Browser dashboard |
| Firecracker | One process for each running VM |
| SQLite | Durable VM, network, and storage records |

## Privilege boundary

The API runs without host network privileges.
It sends typed requests to the network helper.

The helper checks the peer UID.
It derives interface names from UUIDs.

This keeps the privileged surface small.

## State

| Data | Default location |
| --- | --- |
| SQLite database | `data/firecrab.db` |
| VM disks and runtime files | `data/vms/<vm-id>/` |
| Kernels and root filesystems | `images/` |
| Bridge and firewall state | Linux kernel |

Installed systems use `/var/lib/firecrab` as their working directory.

## VM start flow

1. The API changes the VM state to `starting`.
2. It prepares or reuses the VM disk.
3. The helper prepares the network and TAP device.
4. The API starts one Firecracker process.
5. The guest reports `FIRECRAB_NETWORK_READY`.
6. The API changes the VM state to `running`.

The guest signal proves that DHCP and DNS worked inside the VM.

## Limits

firecrab is not a hosted cloud service.
It does not schedule VMs across multiple hosts.

It does not provide automatic failover or availability zones.

## Related

- [Core concepts](concepts.md)
- [Networking](networking.md)
- [Storage](storage.md)
- [Operations](operations.md)
