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
Entries contain the raw bytes returned by the registry; they are not unpacked
or merged into a root filesystem.

Every cache lookup streams the complete entry and verifies both its expected
size and SHA-256 digest before reuse. A corrupt entry is discarded and fetched
again, and a download becomes visible at its final path only after the same
checks succeed.

## Related

- [Images](images.md)
- [API](api.md)
- [Storage](storage.md)
