# OCI images

firecrab can read a container image from a registry and report whether this
host can run it.
The internal pipeline can size a provisioned tree into an ext4 image, pair
it with an architecture-matched kernel, and derive an alias from the reference.
Registering a template is still separate work.

## Inspect

Ask whether a container image can run on this host without downloading its
config or layers.

```sh
curl -s 'http://127.0.0.1:3000/api/oci/inspect?reference=nginx:1.27'
```

The reference is written as at `docker pull`, so a bare name resolves to Docker
Hub's `library` namespace at `latest`.
The answer is the manifest digest this host would pull; an image offering no
manifest for this architecture is rejected, naming the ones it does offer.

OCI platforms use Go's names, so x86_64 appears as `amd64` — not the label the
MicroRegistry catalog uses.
Registries are reached over HTTPS, except `localhost` and `127.0.0.1`.
This endpoint reads registry metadata only. It neither imports the image nor
populates the blob cache.

## Blob cache

Internal OCI pulls cache image configs and layers by their SHA-256 digest at
`<FIRECRAB_IMAGE_ROOT>/.oci/blobs/sha256/<hex>`.
Entries contain the raw bytes returned by the registry and are never replaced
with decompressed data.

Every cache lookup streams the complete entry and verifies both its expected
size and SHA-256 digest before reuse. A corrupt entry is discarded and fetched
again, and a download becomes visible at its final path only after the same
checks succeed.
One config or layer download is limited to 16 GiB by default; operators can
change this with `FIRECRAB_OCI_MAX_BLOB_BYTES`.

## Layer decompression

The internal import pipeline decompresses plain, gzip, and zstd layer streams
into `.oci/layers/sha256/<diff-id>/<compressed-digest>.<codec>.tar`.
Each result keeps the manifest descriptor, whose digest covers the registry
bytes, separate from the matching config `rootfs.diff_ids` entry, whose digest
covers the uncompressed tar stream.

The decoder streams output while calculating that diff ID and publishes a tar
only after it matches. Cache hits are rehashed, corrupt entries are rebuilt
from the verified blob, and failed work leaves no partial tar. Decoder output
is limited to 64 GiB per layer by default; change it with
`FIRECRAB_OCI_MAX_UNCOMPRESSED_LAYER_BYTES`. At most two layer decoders run
process-wide, and each zstd decoder has a 128 MiB window limit.

## Layer safety preflight

Before extraction, the internal pipeline scans every decompressed tar using
GNU long-name/link metadata and PAX overrides. Member names must be relative
paths without parent components; only a directory may name the archive root
as `.` or `./`. Character and block devices are rejected, as are unsupported
special entries. Links must name a target;
hard-link targets are archive-root-relative and cannot be absolute or contain
parent components. Regular whiteout files remain valid for the later merge
stage.

Malformed headers, repeated PAX path/link records, sparse extensions,
missing end records, and truncated member bodies stop the import before any
filesystem tree is created. PAX `size` overrides and global PAX path/link/size
overrides are rejected to avoid different tar parsers disagreeing about member
boundaries or destinations. Each GNU or PAX metadata entry is limited to 1
MiB. Rejection does not delete the already verified compressed blob or
decompressed tar: both remain valid content-addressed cache entries.

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

Activation installs an init at `/sbin/init`, so the image boots on the same
kernel command line every other template uses. It also installs a boot script
that mounts `/proc`, `/sys`, `/dev` and `/run`, brings the interface up before
asking for a lease, reports `FIRECRAB_NETWORK_READY` with the address it
received or `FIRECRAB_NETWORK_FAILED` with a reason, and starts the metrics
agent that reports guest CPU and memory. `/etc/firecrab/services.d` is created
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
A catalog or installed alias is refused.
The ext4 is copied to `rootfs/<alias>.ext4` and registered; a failure deletes that file.

## Service

Entrypoint, Cmd, Env, and WorkingDir become `/etc/firecrab/services.d/app`.
The injected init starts it after the sentinel. It is never PID 1.

## Related

- [Images](images.md)
- [API](api.md)
- [Storage](storage.md)
