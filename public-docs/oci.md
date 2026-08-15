# OCI images

firecrab can inspect a container image and import it as a bootable template.
The pipeline caches layers, merges them, injects a guest runtime, writes ext4,
pairs a kernel, and registers an alias.

## Inspect

Ask whether a container image can run on this host without downloading its
config or layers.

```sh
curl -s 'http://127.0.0.1:3000/api/oci/inspect?reference=nginx:1.27'
```

The reference is written as at `docker pull`, so a bare name resolves to Docker Hub's `library` namespace at `latest`.
The answer is the manifest digest this host would pull; a missing architecture is rejected.
OCI platforms use Go's names, so x86_64 appears as `amd64`.
Registries are reached over HTTPS, except `localhost` and `127.0.0.1`.
This endpoint reads metadata only and includes the alias a later import will claim.

## Import

Import is a background job because REST requests time out at 10 seconds.

```sh
curl -s -X POST http://127.0.0.1:3000/api/oci/import \
  -H 'Content-Type: application/json' \
  -d '{"reference":"nginx:1.27"}'
```

Poll `GET /api/oci/import/{alias}` for the same `ImageInstallResponse` package install uses.
A bad reference is `400 validation_failed`.
A catalog or installed alias is `409 alias_collision`.
A running job for that alias is `409 import_in_progress`.
Success adds the alias to `GET /api/images`.

## Blob cache

Internal OCI pulls cache image configs and layers by their SHA-256 digest at
`<FIRECRAB_IMAGE_ROOT>/.oci/blobs/sha256/<hex>`.
Entries contain the raw bytes returned by the registry and are never replaced with decompressed data.
Every cache lookup verifies size and SHA-256 before reuse; a corrupt entry is discarded and fetched again.
One config or layer download is limited to 16 GiB by default (`FIRECRAB_OCI_MAX_BLOB_BYTES`).

## Layer decompression

The internal import pipeline decompresses plain, gzip, and zstd layer streams
into `.oci/layers/sha256/<diff-id>/<compressed-digest>.<codec>.tar`.
Each result keeps the manifest descriptor, whose digest covers the registry
bytes, separate from the matching config `rootfs.diff_ids` entry, whose digest
covers the uncompressed tar stream.

The decoder publishes a tar only after the diff ID matches.
Cache hits are rehashed and corrupt entries are rebuilt from the verified blob.
Decoder output is limited to 64 GiB per layer (`FIRECRAB_OCI_MAX_UNCOMPRESSED_LAYER_BYTES`).
At most two layer decoders run process-wide, and each zstd decoder has a 128 MiB window.

## Layer safety preflight

Before extraction, the internal pipeline scans every decompressed tar using
GNU long-name/link metadata and PAX overrides. Member names must be relative
paths without parent components; only a directory may name the archive root
as `.` or `./`. Character devices, block devices, and FIFOs are skipped; unsupported special entries are rejected.
Links must name a target; hard-link targets stay archive-root-relative.
Regular whiteout files remain valid for the later merge stage.

Malformed headers, repeated PAX path/link records, sparse extensions,
missing end records, and truncated member bodies stop the import.
PAX `size` overrides and global PAX path/link/size overrides are rejected.
Each GNU or PAX metadata entry is limited to 1 MiB.
Rejection keeps the verified blob and decompressed tar as cache entries.

## Layer merge

The internal merge stage consumes validated, uncompressed tar streams in
manifest order. It reopens each stream without following a cache-path symlink,
then rechecks its exact size, config `diff_id`, and archive safety before
changing the filesystem tree.

The host staging tree preserves ordinary and sticky permissions but remains
owned by the unprivileged API service. It does not activate image-supplied
set-ID bits or apply numeric ownership and extended attributes; those guest
filesystem attributes belong to the later ext4 construction stage.

For each layer, `.wh.<name>` removes the named sibling from lower layers and
`.wh..wh..opq` removes lower-layer children from its directory. Whiteouts are
applied before that layer's ordinary members regardless of archive order, so a
same-layer replacement remains present and marker files never appear in the
result.

Merging builds a private sibling partial tree and atomically publishes it only
after every layer succeeds; the destination must not already exist. Failures
attempt to remove the partial tree and always retain verified blob and
decompressed-layer cache entries.

## Guest toolbox

A container image is an application, not an operating system: it has no PID 1,
no DHCP client, and nothing that reports readiness. The internal pipeline
supplies all three from one static program before a merged tree can become a
bootable image.

That program is taken from a digest-pinned busybox image, pulled through the
same verified stages as the image being imported. It must be a 64-bit
executable for this host with no dynamic loader recorded in it, because a
merged container tree has no loader to satisfy. Only the first import on a host
reaches the registry; the program is then cached at
`<FIRECRAB_IMAGE_ROOT>/.oci/toolbox/`, re-verified on every reuse, and rebuilt
when it no longer passes. Operators can name a mirror with
`FIRECRAB_OCI_TOOLBOX_IMAGE` or an already-present program with
`FIRECRAB_OCI_TOOLBOX_PATH`.

## Guest activation

Activation installs an init at `/sbin/init` and the toolbox at
`/etc/firecrab/busybox` (basename `busybox`, so the multiplexer runs), so the
image boots on the same kernel command line every other template uses. It also
installs a boot script that mounts `/proc`, `/sys`, `/dev` and `/run`, brings
the interface up before asking for a lease, reports `FIRECRAB_NETWORK_READY`
with the address it received or `FIRECRAB_NETWORK_FAILED` with a reason, and
starts the metrics agent that reports guest CPU and memory. When the image
ships util-linux `agetty` and bash, the serial console is
`ttyS0 → agetty → login → bash`; otherwise the injected wrapper prints MOTD
and drops into ash. When the image has a glibc dynamic loader, activation
also copies a digest-pinned official fastfetch (polyfilled, GLIBC_2.17) to
`/usr/bin/fastfetch`. Debian bookworm guests such as `nginx:1.27` have
`apt-get` but no `fastfetch` package, so a guest install is a silent no-op.
The program is cached at `<FIRECRAB_IMAGE_ROOT>/.oci/fastfetch/` after the
first download. Operators can name a host binary with
`FIRECRAB_OCI_FASTFETCH_PATH`. A missing program is not an import failure:
the boot script still tries the guest package manager as a fallback.
`/etc/firecrab/services.d` is created
empty for the image's own entrypoint, which a later stage translates into an
ordinary service under that init rather than PID 1.

Images that place `/sbin` or `/etc` behind a symbolic link are activated
through it.
Resolution is clamped to the tree, and an entry already occupying a guest
path is replaced without writing through it.
A failed activation restores every path it touched.

## Ext4 image

The internal pipeline sizes the ext4 from the provisioned tree rather than
from a fixed image length.
Payload bytes count each regular inode once and include symlink targets;
hard links are not added again.

The image is the payload plus a quarter for ext4 metadata plus 32 MiB of
headroom, rounded up to a whole mebibyte and never below 8 MiB.
One image is limited to 32 GiB by default; operators can change this with
`FIRECRAB_OCI_MAX_ROOTFS_BYTES`.

The file is created sparse, formatted with `mkfs.ext4 -d`, and published
only after `tune2fs` shows free space remains.
A full image, a failed format, or an existing destination is an error;
the partial file is removed and the provisioned tree is left in place.
This stage still pairs no kernel and registers nothing.

## Kernel, name, register

The packed ext4 is paired with this host's no-initrd catalog kernel — Ubuntu.
The alias is the repository and tag — `nginx:1.27` becomes `nginx-1.27`.
A catalog or installed alias is refused; the ext4 is copied to `rootfs/<alias>.ext4`.
That local template is not a MicroRegistry row; register it as in [Images](images.md) and [API](api.md).

## Service

Entrypoint, Cmd, Env, and WorkingDir become `/etc/firecrab/services.d/app`.
The injected init starts it after the sentinel. It is never PID 1.
On start, one `# >>> firecrab vm env` block is rewritten so per-VM `env` overrides image Env (image `export` lines stay; empty map removes the block; missing `services.d/app` is a no-op; plaintext in the guest).

## Related

- [Images](images.md)
- [Dashboard](dashboard.md)
- [API](api.md)
- [Storage](storage.md)
