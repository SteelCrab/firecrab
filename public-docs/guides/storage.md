# Storage

A MicroStorage is a named host directory for VM disks, optionally on a separate mounted device.
Firecrab does not partition, format, or mount disks.
Paths belong to the API host: native Linux or managed Debian on macOS/Windows.

## Register and select

Inspect available roots and mounted filesystems:

```sh
curl -fsS http://127.0.0.1:5523/api/storage
curl -fsS http://127.0.0.1:5523/api/storage/devices
```

Register an absolute directory and use its returned ID as VM `storageRoot`:

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/micro-storages \
  -H 'Content-Type: application/json' \
  -d '{"name":"fast","path":"/mnt/fast"}'
```

The API checks free space before disk preparation.
VM requests accept registered IDs, not arbitrary host paths.
Fixed roots can also be configured with `FIRECRAB_STORAGE_ROOTS='local=data:fast=/mnt/fast'`.
The default root is `data/` under the API working directory.

## Reassign or delete

Storage can change only while a VM is inactive and has no prepared disk.

```sh
curl -fsS -X PUT http://127.0.0.1:5523/api/vms/VM_ID/storage \
  -H 'Content-Type: application/json' \
  -d '{"storageRoot":"STORAGE_ID"}'
```

An existing rootfs returns `409`; Firecrab does not move prepared disks between pools.
A pool cannot be deleted while a VM uses it.
Deleting a pool does not unmount or format its filesystem.

## Installed MicroVM state

A default Linux installation uses this layout:

```text
/var/lib/firecrab/
  data/
    firecrab.db                  resource records
    firecrab.db-wal, -shm        SQLite WAL companion files
    vms/<vm-id>/
      vm.lock
      d/<generation>.ext4       persistent writable disk
      r/<runtime-id>/           per-start configuration, sockets, console, exit
      ssh/                      per-VM SSH keys
  images/
    .templates.json             template registrations
    kernel/, rootfs/            source artifacts
    .packages/                  staged archives and job scratch files
    .microboot/                 builder boot assets
    .oci/                       verified blob, layer, toolbox, and kernel caches
```

SQLite stores lifecycle, resource settings, leases, disk generations, shell pins, and forwarding rules.
Guest filesystems live in ext4 files, outside SQLite.
Selecting another storage root changes VM artifact placement; the database stays in the data directory.
Disk generations survive stop/start; each start has a new runtime directory.
Replacing a source image does not change existing VM disks.

OCI caches keep raw registry bytes separate from verified decoded layers and rehash entries before reuse.
Managed kernels are shared with OCI import and image pairing; remove them through [Kernels](kernels.md).
Metadata-only OCI inspection does not fill the caches.

Inspect installed records with:

```sh
sudo sqlite3 /var/lib/firecrab/data/firecrab.db \
  'SELECT id, name, state, storage_root, disk_generation FROM vms;'
```

## Disk creation

New disks use a copy-on-write reflink when available, otherwise a byte copy.
The resulting disk contents are identical.
Reflinks require XFS or Btrfs on the host and cannot cross filesystems.
Keep source images and VM storage on one filesystem to use them.
`firecrab doctor` checks default/environment roots for a split layout, but not API-registered pools.

## Backup and retention

For a consistent offline backup, stop workload VMs first, then stop the API.
VMs survive API stops, so stopping only the API leaves their disks writable.
Back up these together:

- SQLite database and any WAL companion files.
- Every storage root containing VM artifacts.
- Image directory and `/etc/firecrab/api.env`.

A normal upgrade and ordinary uninstall preserve data and configuration.
`--uninstall --purge` deletes the configured data directory, including default-root disks.
See [Operations](operations.md#uninstall) for the full cleanup behavior.

## Related

- [Installation paths](../getting-started/installation.md#default-linux-paths)
- [Architecture](../reference/architecture.md#persistent-and-runtime-state)
- [Troubleshooting](troubleshooting.md)
