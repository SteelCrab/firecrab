# Storage

A MicroStorage is a named host directory for VM disks.
The directory can be on a separate mounted device.

firecrab does not partition, format, or mount disks.

## Installed MicroVM state

An `install.sh` deployment uses `/var/lib/firecrab` as its working directory by default.
MicroVM control-plane records and VM filesystem artifacts have separate storage lifecycles.

```text
/var/lib/firecrab/
  data/
    firecrab.db
    firecrab.db-wal              # present while WAL has live frames
    firecrab.db-shm              # present while SQLite is open in WAL mode
    vms/
      <vm-id>/
        d/<generation>.ext4
        r/<runtime-id>/
          fc.json
          fc.sock
          console.log
  images/
    .templates.json              # persisted runtime template registrations
    .microboot/                  # Alpine netboot kernel, initramfs, and placeholder disk
    .oci/blobs/sha256/<hex>       # verified raw OCI config and layer blobs
    .oci/layers/sha256/<diff-id>/ # verified uncompressed layer tar streams
    .oci/kernel/<arch>/<image>    # digest-pinned managed guest kernels
    .packages/                   # staged M2Image archives and origin markers
    kernel/
    rootfs/
```

`data/firecrab.db` is the SQLite database for VM identity, lifecycle state, image version and hashes, CPU, RAM, disk size, network, storage selection, disk generation, Shell pins, and port forwards.
The guest filesystem is not stored in SQLite.
It is the writable ext4 file under the selected storage root's `vms/<vm-id>/d/` directory.

The `images/` directory is the default `FIRECRAB_IMAGE_ROOT` and contains immutable source templates used when preparing new VM disks.
Replacing a source image does not modify a VM disk that was already prepared.
Temporary download, package-build, and bootstrap scratch files can also appear under `.packages/` while a job is running or after an interrupted job.
OCI config and layer blobs stay as raw registry bytes under `.oci/blobs/sha256/`; verified tar streams stay separately under `.oci/layers/sha256/`, retaining both the uncompressed config diff ID and compressed manifest digest. Both caches rehash entries before reuse.
Managed kernel images share `.oci/kernel/<arch>/` between the Kernels page,
OCI import, and image kernel updates. They are deleted only through kernel
management and only after all image references are removed.
`GET /api/oci/inspect` reads metadata only and does not populate this cache.

Other installed and runtime files live outside `/var/lib/firecrab`.

| Path | Purpose |
| --- | --- |
| `/etc/firecrab/api.env` | Operator-owned API configuration |
| `/etc/systemd/system/firecrab-*.service` | Installed systemd units |
| `/usr/local/lib/firecrab/` | API, network helper, and `extract-vmlinux` |
| `/usr/local/bin/firecrab` | Host CLI (diagnostics, status, and VM/network/image operations) |
| `/usr/local/share/firecrab/dashboard/` | Installed dashboard assets |
| `/run/firecrab/` | Ephemeral helper socket, dnsmasq configuration, PID, hosts, and leases |
| systemd journal and Linux networking state | Service logs, bridges, TAPs, nftables, and live processes |

Inspect the default installed state with these commands.

```sh
sudo sqlite3 /var/lib/firecrab/data/firecrab.db \
  'SELECT id, name, state, storage_root, disk_generation FROM vms;'
sudo find /var/lib/firecrab/data/vms -maxdepth 4 -type f
```

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
curl -s http://127.0.0.1:5523/api/storage/devices
```

This endpoint lists mounted filesystems and free space.
It does not change the host.

## Register

```sh
curl -s -X POST http://127.0.0.1:5523/api/micro-storages \
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
curl -s -X PUT http://127.0.0.1:5523/api/vms/<vm-id>/storage \
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
The runtime directory contains one Firecracker configuration, API socket, and serial-console log for that start.

The SQLite database remains under the firecrab data directory when a VM uses an environment root or MicroStorage.
Only that VM's `d/` and `r/` artifact directories move to the selected storage root.

## Upgrade, uninstall, and backup

A normal `./install.sh` upgrade preserves the SQLite database, VM artifacts, installed images, and `api.env`.
`./install.sh --uninstall` also preserves those files.
`./install.sh --uninstall --purge` removes the configured data directory and therefore deletes the database and default-root VM disks.

Stop `firecrab-api` before taking an offline backup.
Back up `firecrab.db`, every configured storage root containing VM artifacts, and the image directory together so database references and files remain consistent.

## Disk creation

A VM disk begins as a copy-on-write clone of its template, falling back to a byte copy if the host refuses; the disk is identical either way.

Reflinks need XFS or Btrfs and cannot cross filesystems — that is the filesystem holding the `.ext4` files, not the one inside them, so keep the image root and every storage root on one.
`firecrab doctor` checks the default roots and `FIRECRAB_STORAGE_ROOTS` for a split layout; pools registered through the API are not inspected.

## Delete

A pool cannot be deleted while a VM uses it.
Deleting a pool does not unmount or format the host filesystem.

## Related

- [Core concepts](concepts.md)
- [API](api.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [Troubleshooting](troubleshooting.md)
