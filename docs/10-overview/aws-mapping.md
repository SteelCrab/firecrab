# AWS comparison

This table helps readers who know AWS terms.
The systems are not exact equivalents.

## Resource mapping

| firecrab | Rough AWS comparison | Important difference |
| --- | --- | --- |
| MicroVM | EC2 instance | Runs on one user-managed host |
| M2Image | AMI | Stored as local kernel and rootfs files |
| MicroNetwork | VPC subnet | Uses a local Linux bridge |
| Egress policy | Internet gateway and route policy | Controls local NAT and filtering |
| MicroStorage | EBS placement choice | Points to an existing host mount |
| Browser terminal | EC2 serial console | Uses the VM serial stream |

## Lifecycle mapping

| firecrab action | Rough AWS action |
| --- | --- |
| Create VM | Run an instance record without starting it |
| Start VM | Start an instance |
| Stop VM | Stop an instance |
| Delete VM | Terminate an instance and delete its local disk |

## What firecrab does not provide

- Multi-host scheduling
- Automatic failover
- Availability zones
- Managed load balancing
- Managed object storage
- A public cloud control plane

firecrab is designed for a private single-host environment.
Use the comparison only as a naming aid.
