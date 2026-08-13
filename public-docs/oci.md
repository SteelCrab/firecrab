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

Tar member validation, extraction, whiteout handling, and layer merging remain
separate import stages. Inspect does not run decompression or fill either OCI
cache.

## Related

- [Images](images.md)
- [API](api.md)
- [Storage](storage.md)
