# Core concepts

firecrab uses a small resource model.
Each resource has one clear job.

## MicroVM

A MicroVM is a small virtual machine started by Firecracker.
Each running VM has one Firecracker process.

## MicroNetwork

A MicroNetwork is a named IPv4 subnet.
It owns a bridge, DHCP settings, and an internet policy.

Every VM must belong to one MicroNetwork.
There is no hidden default network.

## MicroStorage

A MicroStorage is a registered host directory.
VM disk files can be placed in that directory.

firecrab does not format or mount physical disks.

## M2Image

An M2Image contains a kernel and root filesystem.
It is the source template for a new VM disk.

Only installed images can create VMs.

## TAP and bridge

A bridge is a virtual network switch on the host.
Each MicroNetwork has its own bridge.

A TAP interface connects one VM to its bridge.
Bridge names start with `mnb`.
TAP names start with `fct`.

## Egress policy

An egress policy controls traffic leaving a VM.
The values are `internet` and `isolated`.

The MicroNetwork also has an `internetEnabled` switch.
Both settings must allow internet traffic.

## Disk generation

A disk generation is the durable writable rootfs for one VM.
It survives stop and start.

A runtime directory belongs to one start attempt.
It contains the Firecracker socket, configuration, and console log.

## Related

- [Architecture](architecture.md)
- [API](api.md)
- [Networking](networking.md)
- [Storage](storage.md)
