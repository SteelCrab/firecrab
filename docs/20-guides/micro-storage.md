# MicroStorage

A MicroStorage is a named host directory for VM disks.
The directory can be on a separate mounted disk.

firecrab does not partition, format, or mount disks.
Prepare the host filesystem before registering it.

## Storage sources

`GET /api/storage` combines three sources.

| Source | ID | Configuration |
| --- | --- | --- |
| Default | `default` | `data/` |
| Environment | Operator name | `FIRECRAB_STORAGE_ROOTS` |
| MicroStorage | UUID | API or dashboard registration |

Use an environment list for fixed deployment paths.

```sh
FIRECRAB_STORAGE_ROOTS='local=data:fast=/mnt/fast' cargo run -p firecrab-api
```

## Find mounted devices

```sh
curl -s http://127.0.0.1:3000/api/storage/devices
```

This endpoint reports mounted filesystems and free space.
It does not change the host.

## Register a pool

```sh
curl -s -X POST http://127.0.0.1:3000/api/micro-storages \
  -H 'Content-Type: application/json' \
  -d '{"name":"fast","path":"/mnt/fast"}'
```

The path must be absolute.
The API creates the final directory when its parent is valid.

List all selectable roots after registration.

```sh
curl -s http://127.0.0.1:3000/api/storage
```

## Place a VM

Set `storageRoot` when creating a VM.
Use the ID from `GET /api/storage`.

```json
{
  "name": "worker-1",
  "template": "alpine-3.24",
  "cpu": 1,
  "ram": 512,
  "diskGb": 2,
  "microNetworkId": "<network-id>",
  "storageRoot": "<storage-id>"
}
```

The API checks free capacity before disk preparation.
Clients cannot send an arbitrary host path in a VM request.

## Reassign before disk creation

```sh
curl -s -X PUT http://127.0.0.1:3000/api/vms/<vm-id>/storage \
  -H 'Content-Type: application/json' \
  -d '{"storageRoot":"<storage-id>"}'
```

The VM must be inactive.
Reassignment returns `409 storage_has_disk` after a rootfs exists.

firecrab does not silently copy an existing VM disk to another pool.

## Disk layout

Each VM has durable disk data and per-start runtime data.

```text
<storage-root>/vms/<vm-id>/
  d/<generation>.ext4
  r/<runtime-id>/
    fc.json
    fc.sock
    console.log
```

The disk generation survives stop and start.
A new runtime directory is created for each start.

## Delete a pool

```sh
curl -i -X DELETE http://127.0.0.1:3000/api/micro-storages/<storage-id>
```

Deletion returns `409 storage_in_use` while a VM points to the pool.
Deleting a pool does not format or unmount its host filesystem.

## Related documents

- [REST API](api.md)
- [Installation](install.md)
- [Architecture](../10-overview/architecture.md)
