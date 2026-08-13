# OCI images

firecrab can read a container image from a registry and report whether this
host can run it.
Importing one into a bootable rootfs is separate work and not available yet.

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
as `.` or `./`. Character and block devices are
rejected, as are unsupported special entries. Links must name a target;
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

Registry inspection, raw blob caching, decompression, safety validation, and
merge remain distinct import stages. `GET /api/oci/inspect` stops at metadata
and fills no OCI cache; caching and decompression do not publish a merged tree.

## Related

- [Images](images.md)
- [API](api.md)
- [Storage](storage.md)
