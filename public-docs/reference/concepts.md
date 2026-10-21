# Core concepts

| Term | Meaning |
| --- | --- |
| MicroVM | Small VM with one Firecracker process |
| microManager | Managed Debian runtime used on macOS and Windows |
| MicroNetwork | Named IPv4 subnet with bridge, DHCP, and internet policy; optional IPv6 |
| MicroStorage | Registered host directory for VM disks; Firecrab does not format or mount it |
| M2Image | Kernel and rootfs template used to prepare a VM disk |
| MicroRegistry | Image/kernel catalog and package source; custom registrations stay local |
| MicroBoot | API-only image builder using a temporary VM |
| Bridge | Host virtual switch; MicroNetwork names start with `mnb` |
| TAP | Interface connecting one VM to its bridge; names start with `fct` |
| Egress policy | VM outbound policy: `internet` or `isolated` |
| Disk generation | Persistent writable VM rootfs; survives stop/start |
| Runtime directory | Files for one start attempt: configuration, sockets, console, and exit record |
| Shim | Process that owns Firecracker and keeps its console/lifecycle independent of the API |

Every VM requires an explicit MicroNetwork and an installed image.
Internet access requires both network `internetEnabled` and VM `egressPolicy` to allow it.

## Related

- [Architecture](architecture.md)
- [Networking](../guides/networking.md)
- [Storage](../guides/storage.md)
