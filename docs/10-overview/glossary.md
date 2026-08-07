# Glossary

## Resources

**MicroVM**

A small virtual machine started by Firecracker.
Each VM has its own Firecracker process.

**MicroNetwork**

A named IPv4 subnet for one or more microVMs.
It owns a bridge, DHCP settings, and an internet policy.

**MicroStorage**

A registered host directory used for VM disks.
It points to a directory that is already mounted.

**M2Image**

A kernel and root filesystem template used to create microVMs.
Only installed images can create VMs.

**Builder VM**

A temporary VM used to build or update an M2Image.

## Network terms

**Bridge**

A Linux virtual switch used by a MicroNetwork.

**TAP**

A host network interface connected to one microVM.

**Lease**

The stored IPv4, MAC address, and hostname assigned to a VM.

**Egress policy**

A rule that allows or blocks traffic from a VM to the internet.

## Runtime terms

**Template**

An immutable source image.
firecrab copies it before a VM writes data.

**Disk generation**

The durable writable root filesystem for one VM.
It is reused after a stop and start.

**Runtime**

Files created for one VM start attempt.
They include the Firecracker configuration, socket, and console log.

**Sentinel**

A known line written to the serial console by a guest script.
firecrab uses sentinels to detect readiness or failure.

## Service terms

**API**

The unprivileged `firecrab-api` process.

**Network helper**

The small privileged process that changes host networking.

**Artifact ledger**

Stored metadata that identifies the disk generation and last runtime.
