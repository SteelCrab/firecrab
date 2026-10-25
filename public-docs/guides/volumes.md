# Persistent data volumes

MicroBlockStorage is a named ext4 data disk with a lifecycle independent of its MicroVM.
MicroStorage selects the host directory that contains the disk.
VM deletion releases the attachment and preserves the volume.

## Create and use

Create a volume, then select it when creating a VM:

```sh
firecrab volume create app-data --size 10
firecrab vm create --name app --template IMAGE_ALIAS --network NETWORK_ID \
  --volume app-data:/data
firecrab vm start VM_ID
```

The volume starts empty and is mounted before the managed application runs.
Files already present at the target path are hidden while the volume is mounted.
Set application ownership and permissions inside the guest as required.
There is no automatic copy of image files into an empty volume.

The dashboard has a **MicroBlockStorage** page for creation and deletion.
Use the **Volumes** tab in an inactive VM's details to manage attachments.
Select **Attach volume**, choose an available volume, and enter its path inside the VM.
To release a disk, select **Detach** and confirm **Detach volume**. Its data is kept.
The VM creation form can also select existing volumes.

## Move data between VMs

Stop the owning VM before detaching:

```sh
firecrab vm stop VM_A
firecrab volume detach app-data
firecrab volume attach app-data --vm VM_B --mount-path /data
firecrab vm start VM_B
firecrab volume inspect app-data --json
```

Deleting an inactive VM also releases its volumes without deleting their files.
Deleting an attached volume returns a conflict.
To remove its data permanently, detach it and run:

```sh
firecrab volume delete app-data
```

## API

| Method | Path | Body or result |
| --- | --- | --- |
| POST | `/api/microblockstorage` | `{"name":"app-data","sizeGib":10,"storageRoot":null}` |
| GET | `/api/microblockstorage` | List volumes |
| GET | `/api/microblockstorage/{id}` | Volume metadata and attachment |
| POST | `/api/microblockstorage/{id}/attach` | `{"vmId":"VM_UUID","mountPath":"/data"}` |
| POST | `/api/microblockstorage/{id}/detach` | No body |
| DELETE | `/api/microblockstorage/{id}` | Delete an unattached volume |

VM creation accepts `blockStorages: [{"volumeId":"VOLUME_UUID","mountPath":"/data"}]`.
All requested volumes are reserved together. A reservation conflict leaves them unchanged.
The CLI accepts a volume name or UUID. API paths use UUIDs.

## Limits and recovery

- A volume belongs to one VM, including while that VM is stopped.
- A VM can have at most eight data volumes with distinct, non-overlapping mount paths.
- Attach and detach require an inactive VM and a released VMM ownership lock.
  Running, paused, and transitioning VMs cannot change attachments.
- Pool members, builder VMs, and MicroService replicas cannot own volumes in this version.
- Mount paths must be absolute data directories without spaces, symlinks, or dot components.
  Boot, system configuration, runtime, and Firecrab state directories are reserved.
- Guests must use the Firecrab OCI runtime, systemd, or OpenRC.
  Native init uses required fstab entries. The OCI runtime checks UUID-based mounts
  before network readiness and application startup. A mount failure fails startup.
- Data disks use Firecracker's `Writeback` cache setting to honor guest flush requests.
  This does not replace application backups or guarantee survival of faulty host storage.
- API restarts retain attachments. Interrupted disk creation/deletion is reconciled at startup.
  A missing or mismatched disk fails VM start rather than creating a replacement.
- Hotplug, shared writers, host directory mounts, snapshots, and online expansion are separate work.

## Files and backup

The disk lives at `<MicroStorage root>/volumes/<volume UUID>/data.ext4`.
Paths refer to the API host, including managed Linux on macOS and Windows.
The disk is not a shared macOS or Windows host directory.
A MicroStorage containing volumes cannot be deleted.

For a consistent offline backup, stop the workload VM before copying its volume.
Back up the SQLite database and storage roots together.
See [Storage](storage.md#backup-and-retention).
