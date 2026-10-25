# Kernel management

Firecrab builds guest kernels on the Linux host from kernel.org's current `stable` release.
Kernel installation and OCI import share the same verified cache.
Kernel installation does not change the kernels assigned to existing images.

## Catalog

`GET /api/kernels` lists the latest upstream stable release and locally cached versions.
Release discovery uses `https://www.kernel.org/releases.json` and caches metadata for one hour.
Only the `stable` entry is selected; `mainline`, release candidates, and `longterm` entries are excluded.
Cached metadata and installed kernels remain available during upstream outages.

The cache lives at `<FIRECRAB_IMAGE_ROOT>/.oci/kernel/<architecture>/`.
The host builds `vmlinux` for `x86_64` and `Image` for `aarch64`.
Required block, network, ext4, serial, and virtio-vsock drivers are built in.
No initrd or guest module tree is required.

Each successful build stores the kernel, its `.config`, and a `.json` build receipt.
The receipt records the source URL, source SHA256, image SHA256, architecture, and build revision.
Image digests and architecture are verified before cache reuse.
Previously installed Firecrab kernels remain usable when their compiled legacy digests match.
New installations never download a kernel package from `registry.firecrab.dev`.

## Host requirements

Install a native C compiler, make, flex, bison, bc, Perl, tar, xz, and development headers.
For Debian and Ubuntu:

```sh
sudo apt-get install build-essential flex bison bc perl xz-utils libssl-dev libelf-dev
```

The first installation downloads source and compiles the kernel.
Allow enough free disk space for the source tree and compiler output.
Build files are removed after each completed or failed attempt; the verified source archive is retained.
The API must have permission to write to `FIRECRAB_IMAGE_ROOT`.

`FIRECRAB_KERNEL_BUILD_JOBS` controls compiler parallelism and defaults to `2`.
The host runs one kernel compilation at a time and stops a build after two hours.
Builds require a Linux host with the requested CPU architecture.

## Install

Select the current stable version or request `stable`:

```sh
curl -s http://127.0.0.1:5523/api/kernels
curl -s -X POST http://127.0.0.1:5523/api/kernels/stable/install
curl -s http://127.0.0.1:5523/api/kernels/stable/install
```

A concrete final release such as `7.2.12` can also be requested.
The POST returns `202`; GET reports `running`, `succeeded`, or `failed` with build logs.
A verified installed kernel is reused. A corrupt cache is rebuilt.
Source archives are checked against kernel.org's published SHA256 before extraction.

`FIRECRAB_KERNEL_RELEASES_URL` changes the release metadata endpoint.
Set it to `none` to disable source downloads and allow only cached kernels or an operator override.
Source URLs must identify the corresponding archive on `https://cdn.kernel.org`.
`FIRECRAB_IMAGE_BASE_URL` controls image packages independently and does not select the kernel source.

API compatibility fields describe the source build:

- `packageUrl` is the upstream source archive URL when downloads are enabled.
- `packageSha256` is the source archive SHA256 after a build.
- `imageSha256` is the image SHA256 after a build, or the known legacy image digest.
- Digests that are not yet known are empty strings.

## OCI import and offline hosts

OCI import resolves the current stable release and builds it if needed.
If upstream access or compilation fails, it can reuse a verified installed kernel.
An installed catalog image with a compatible kernel remains the final fallback.
`FIRECRAB_OCI_KERNEL_PATH` selects a local kernel without requiring upstream access.
The supplied file must be regular, have the host architecture, and be smaller than 256 MiB.
Firecrab copies and hashes it into a separate cache entry before use.

The manual `scripts/firecracker-menual/install-linux-kernel.sh` entry point uses the same native builder.
It checks source SHA256 and preserves the filenames used by the manual console scripts.
The older `scripts/build-guest-kernel.sh` and `packaging/guest-kernel.json` remain for reproducing legacy published artifacts.
They are not used by the API's host build path.

## Update an image

An installed image detail record exposes its kernel filename and digest.
Managed-kernel images also expose `kernelVersion`.
Use a concrete installed version when changing an image:

```sh
curl -s -X PUT http://127.0.0.1:5523/api/images/ubuntu-26.04/kernel \
  -H 'Content-Type: application/json' \
  -d '{"kernelVersion":"7.2.12"}'
```

The selected kernel must already be installed and verified.
The operation changes the image's kernel and retains its rootfs and optional initrd.
It returns `409 in_use` while an instance VM references the image.
Installing a newer stable release does not automatically update existing images.

## Delete

`DELETE /api/kernels/{version}` removes a concrete local version and its build metadata.
Deletion returns `409 in_use` while an installed image references the kernel.
Deleting an image does not remove its independently managed kernel cache.

## Related

- [Images](images.md)
- [OCI images](oci.md)
- [API](../reference/api.md)
- [Storage](storage.md)
- [Operations](operations.md)
