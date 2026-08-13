# Storage

A MicroStorage is a named host directory for VM disks.
The directory can be on a separate mounted device.

firecrab does not partition, format, or mount disks.

## Storage sources

`GET /api/storage` combines these sources.

| Source | Configuration |
| --- | --- |
| Default | `data/` |
| Environment | `FIRECRAB_STORAGE_ROOTS` |
| MicroStorage | API or dashboard registration |

Use environment roots for fixed host paths.

```sh
FIRECRAB_STORAGE_ROOTS='local=data:fast=/mnt/fast' \
  cargo run -p firecrab-api
```

## Find mounts

```sh
curl -s http://127.0.0.1:3000/api/storage/devices
```

This endpoint lists mounted filesystems and free space.
It does not change the host.

## Register

```sh
curl -s -X POST http://127.0.0.1:3000/api/micro-storages \
  -H 'Content-Type: application/json' \
  -d '{"name":"fast","path":"/mnt/fast"}'
```

The path must be absolute.
Use the returned UUID as a storage ID.

## Place a VM

Set `storageRoot` in the VM create request.
Use an ID from `GET /api/storage`.

The API checks available space before preparing the disk.
A VM request cannot contain an arbitrary host path.

## Reassign

Storage can change before the VM disk exists.

```sh
curl -s -X PUT http://127.0.0.1:3000/api/vms/<vm-id>/storage \
  -H 'Content-Type: application/json' \
  -d '{"storageRoot":"<storage-id>"}'
```

The VM must be inactive.
The API returns `409` after a rootfs exists.

firecrab does not copy an existing disk between pools.

## Disk layout

```text
<storage-root>/vms/<vm-id>/
  d/<generation>.ext4
  r/<runtime-id>/
    fc.json
    fc.sock
    console.log
```

The disk generation survives stop and start.
Each start gets a new runtime directory.

## Disk creation

A VM disk begins as a copy-on-write clone of its template.
firecrab asks the host filesystem for a reflink so both share the same blocks.

Reflinks need a reflink-capable host filesystem such as XFS or Btrfs.
This is the filesystem holding the `.ext4` files, not the ext4 filesystem inside them.

A reflink cannot cross filesystems.
Keep the image root and the storage root on one filesystem.

firecrab falls back to a full byte copy whenever the host refuses.
The disk is identical either way; only creation speed and disk usage differ.

Run `firecrab-doctor` to check for a split layout.

## Delete

A pool cannot be deleted while a VM uses it.
Deleting a pool does not unmount or format the host filesystem.

## Related

- [Core concepts](concepts.md)
- [API](api.md)
- [Installation](installation.md)
- [Troubleshooting](troubleshooting.md)
