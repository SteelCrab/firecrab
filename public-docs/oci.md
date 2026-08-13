# OCI images

firecrab can read a container image from a registry and report whether this
host can run it.
Importing one into a bootable rootfs is separate work and not available yet.

## Inspect

Ask whether a container image can run on this host before downloading it.

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
This endpoint reads metadata only and imports nothing.

## Related

- [Images](images.md)
- [API](api.md)
- [Storage](storage.md)
