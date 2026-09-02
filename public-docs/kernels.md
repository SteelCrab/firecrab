# Kernel management

firecrab manages digest-pinned guest kernels independently from M2Image rootfs files.
The same cache is used by the dashboard, OCI import, and image kernel updates.

## Contents

- [Catalog](#catalog)
- [Install](#install)
- [Update an image](#update-an-image)
- [Delete](#delete)
- [Related](#related)

## Catalog

`GET /api/kernels` lists the host architecture's supported releases and local cache state.
Linux `7.2.2` is the newest release in this build.
Linux `7.1.9` remains available for compatibility testing and rollback.
Each row includes the architecture, kernel filename, package digest, image digest, and usage state.

The cache lives at `<FIRECRAB_IMAGE_ROOT>/.oci/kernel/<architecture>/`.
An entry is marked installed only after the package and unpacked image pass digest and architecture checks.

## Install

Set `FIRECRAB_IMAGE_BASE_URL` to the MicroRegistry or a compatible private mirror.
The default is `https://registry.firecrab.dev`.

```sh
curl -s -X POST http://127.0.0.1:5523/api/kernels/7.2.2/install
curl -s http://127.0.0.1:5523/api/kernels/7.2.2/install
```

The POST returns `202` and the GET returns `running`, `succeeded`, or `failed` with an operator log.
The job is idempotent for a verified cache and refetches a corrupt or incomplete cache.
Set the base URL to `none` to disable remote downloads.

## Update an image

An installed image detail record exposes its kernel filename and digest.
Managed-kernel images also expose `kernelVersion`.

```sh
curl -s -X PUT http://127.0.0.1:5523/api/images/ubuntu-26.04/kernel \
  -H 'Content-Type: application/json' \
  -d '{"kernelVersion":"7.2.2"}'
```

The selected kernel must already be installed and verified.
The operation changes the alias pin and keeps the rootfs and optional initrd.
It returns `409 in_use` while an instance VM references the image.

## Delete

`DELETE /api/kernels/{version}` removes one local cache entry.
Deletion returns `409 in_use` while an installed image references the kernel.
Deleting an image does not remove its independently managed kernel cache.

## Related

- [Images](images.md)
- [OCI images](oci.md)
- [API](api.md)
- [Storage](storage.md)
- [Operations](operations.md)
