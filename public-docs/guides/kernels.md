# Kernels

Firecrab downloads prebuilt guest kernels from `https://registry.firecrab.dev/kernel/catalog.json`.
The catalog supplies versions, architecture, package checksum, publication date, and EOL status.
The dashboard lists the three newest published versions for the host architecture.
Locally installed versions remain visible for rollback.
Versions are read from the catalog, not hardcoded in the application.

## Installation

Select a version in **Kernels** and choose **Download kernel**.
The detail panel shows package resolution, download, extraction, verification, and installation.
Download progress uses received bytes and the server's content length.
Other stages show their current activity without an estimated percentage.
Completed installations no longer show a progress panel.

The API supports concrete catalog versions and the `stable` alias:

```sh
curl -s http://127.0.0.1:5523/api/kernels
curl -s -X POST http://127.0.0.1:5523/api/kernels/stable/install
curl -s http://127.0.0.1:5523/api/kernels/stable/install
```

The POST returns `202`. GET returns status, log, timing, and download bytes.
The dashboard follows an existing job after refresh instead of starting a second installation.

Installation verifies the compressed package SHA256, image architecture, and built-in runtime drivers.
Vsock drivers are required for VM terminal communication.
Packages with missing or modular required drivers are rejected before publication to the local cache.
No compiler, source download, initrd, or guest module installation is required.

Each successful installation stores an image, its `.config`, and a `.json` receipt.
The receipt records package URL, package SHA256, upstream source SHA256, image SHA256, and architecture.
Previously verified native builds and pinned legacy kernels remain usable.
Existing images keep their assigned kernel until explicitly changed.

## Registry and offline operation

`FIRECRAB_KERNEL_BASE_URL` selects the kernel registry base URL.
Set it to `none` to disable downloads and use verified cached kernels or an operator override.
The legacy setting `FIRECRAB_KERNEL_RELEASES_URL=none` also disables downloads unless the new setting is explicit.
The old source metadata URL and `FIRECRAB_KERNEL_BUILD_JOBS` no longer control API installations.

`FIRECRAB_IMAGE_BASE_URL` controls image packages independently.
Changing the kernel registry does not change image package management.

Registry metadata is cached for five minutes and reused during network outages.
Catalog caches are bound to their registry URL.
A verified installed version can be reused without network access.
OCI import resolves a published kernel or reuses a verified installed fallback.
The operator override `FIRECRAB_OCI_KERNEL_PATH` remains available.

API fields:

- `installSource: "registry"` identifies the prebuilt installation path.
- `publishedAt` is the registry package publication timestamp.
- `releasedAt`, when present from an older service, is the upstream release date.
- `packageUrl` and `packageSha256` identify the registry archive.
- `imageSha256` identifies the installed kernel image.

## Cancellation and deletion

`DELETE /api/kernels/{version}/install` requests cancellation.
The job remains `running` with `cancelling: true` until temporary files are removed.
Its final status is `cancelled` unless the short final publication already completed.

`DELETE /api/kernels/{version}` stops an unfinished installation and removes that version's managed files.
It preserves other versions and manually named files.
Deletion returns `409 in_use` while an installed image references the kernel.
Deleting an image does not remove the independent kernel cache.

## Related

- [Images](images.md)
- [OCI images](oci.md)
- [API](../reference/api.md)
- [Operations](operations.md)
