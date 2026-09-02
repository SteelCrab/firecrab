# Images

An M2Image contains a kernel and root filesystem.
It is the source template for new VM disks.
A container image from a registry is imported separately; see [OCI images](oci.md).

Supported aliases are `alpine-3.24.1`, `ubuntu-26.04`, and `rocky-9.8`.
All three support x86_64 and ARM64. Rocky is exposed only through the
versioned `rocky-9.8` alias.
ARM64 packages keep the distro PE32+ `Image`; x86_64 packages use an ELF
`vmlinux`. Firecracker cannot boot an x86_64 kernel on an ARM64 host.

Registration reads the kernel's own header and rejects a mismatch.
A kernel in a format firecrab cannot classify is registered without that check.

A rootfs carrying no `/lib/modules` can only boot on a kernel with the boot
path built in: `virtio_blk`, `virtio_net`, `virtio_mmio`, and `ext4`.
Firecracker has no PCI, so the virtio drivers must be the MMIO variants.
An image whose kernel keeps those as modules needs the matching initrd.

## Build

Build all images supported by the current Linux host architecture.

```sh
./scripts/build-m2images.sh
```

Build one alias when needed.

```sh
./scripts/build-m2images.sh --alias alpine-3.24.1
./scripts/build-m2images.sh --alias rocky-9.8
```

Build each architecture on a native host of the same architecture.

```sh
./scripts/build-m2images.sh --arch x86_64
./scripts/build-m2images.sh --arch aarch64
```

All release builders create ext4 directly with `mkfs.ext4 -d`; Docker is not
used. Alpine and Ubuntu unpack their official base tarballs. Rocky 9.8
downloads the official Container-Base archive as a tarball and uses Rocky's
own `dnf` in a temporary privileged chroot; no container runtime is involved.
The exact Rocky Container-Base build is pinned by `ROCKY_CONTAINER_BUILD` in
`packaging/m2images.json` alongside the distribution version.

The host builder uses `sudo` for chroot mounts and ownership-preserving
extraction. Build x86_64 on x86_64 and ARM64 on ARM64; foreign-architecture
chroots are rejected with an explicit error.

## Output

```text
dist/m2images/
  catalog.json
  x86_64/
    alpine-3.24.1.tar.zst
    ubuntu-26.04.tar.zst
    rocky-9.8.tar.zst
    SHA256SUMS
  aarch64/
    alpine-3.24.1.tar.zst
    ubuntu-26.04.tar.zst
    rocky-9.8.tar.zst
    SHA256SUMS
```

Verify packages after building them.

```sh
cd dist/m2images/x86_64
sha256sum -c SHA256SUMS
tar --list --zstd --file alpine-3.24.1.tar.zst
```

## Release manifest

`packaging/m2images.json` is the source of truth for distribution versions,
release revisions, builders, artifact filenames, boot arguments, and R2
object keys. `packaging/m2images.schema.json` documents its shape, and the
same manifest is compiled into `firecrab-api` so runtime paths cannot drift
from the build scripts.

Validate it before a release.

```sh
python3 scripts/m2image-manifest.py validate
./scripts/build-m2images.sh --list
```

To update a distribution, change its `series`, `version`, `revision`, builder
environment, artifacts, and registry keys in the manifest. Use a new alias
when compatibility changes (for example `rocky-9.8` to `rocky-9.9`); bump only
`revision` when rebuilding the same pinned distribution release.

## Publish to Cloudflare R2

Release packages are uploaded with [rclone to Cloudflare R2](publish.md).

## Bootstrap

The dashboard can build a supported image in a temporary builder VM.
This path does not need Docker or a host chroot.

The builder downloads distribution files and creates an ext4 rootfs.
firecrab stops the builder before reading its disk.

Only one bootstrap job can run at a time.
The builder is removed after success, failure, or cancellation.

Rocky bootstrap downloads the pinned official Container-Base archive into
MicroBoot and does not require an already-installed Rocky template or Docker.

## Install

Only installed images appear in the VM create form.
Image files live below `FIRECRAB_IMAGE_ROOT`.

By default the API uses the public MicroRegistry.
Set `FIRECRAB_IMAGE_BASE_URL` to use another package source, or set it to
`none` to disable remote installs.

MicroRegistry selects packages matching the host architecture.
Every catalog entry must declare either `x86_64` or `aarch64` explicitly.

The API downloads and validates a package before installing it.
Deleting an installed image does not delete its staged package.

The API validates paths and artifacts before use.
Restart the API after replacing files outside the API workflow.

## Details and kernel management

`GET /api/images/{alias}` returns the same image record as the catalog with
the current kernel filename, kernel SHA-256, rootfs SHA-256, optional initrd
SHA-256, and `kernelVersion` when the image uses a managed kernel.

`GET /api/kernels` lists the host architecture's digest-pinned kernel catalog.
The current release includes Linux `7.2.2` as the newest entry and keeps
`7.1.9` available for rollback or compatibility checks.

Install a kernel independently from any image.

```sh
curl -s -X POST http://127.0.0.1:5523/api/kernels/7.2.2/install
curl -s http://127.0.0.1:5523/api/kernels/7.2.2/install
```

The install job downloads and verifies the package, then stores one kernel at
`.oci/kernel/<architecture>/`. A cached kernel is reusable by OCI imports and
image updates.

After a kernel is installed, update an installed image with:

```sh
curl -s -X PUT http://127.0.0.1:5523/api/images/ubuntu-26.04/kernel \
  -H 'Content-Type: application/json' \
  -d '{"kernelVersion":"7.2.2"}'
```

The update changes the image alias's kernel pin and leaves its rootfs and
initrd unchanged. It is refused while an instance VM references the image,
or when the selected kernel is not installed and verified.

`DELETE /api/kernels/{version}` removes an installed kernel only when no
installed image references it. Deleting an image does not remove a managed
kernel cache, so kernel lifecycle remains independent.

## Register

Register when an installed custom alias should appear in this host's MicroRegistry list.
`POST /api/microregistry/register` packs the template and writes a SQLite catalog row on this host.
The row is not published to any remote registry, including `registry.firecrab.dev`.
Success stages `{alias}.tar.zst` locally and the listing marks the row `downloadable`.
Remote Download and Install still accept only release aliases, so they do not reinstall that custom row.
A foreign or unsupported kernel fails the job with no row or archive.
An unclassifiable kernel is still accepted.
A success survives restart.
A catalog outage still lists the local rows.

## Add an alias

For a new distribution family, add its manifest entry and builder, then update:

- Bootstrap guest script mapping
- CI boot matrix in `.github/workflows/ci.yml`

## Related

- [OCI images](oci.md)
- [Kernel management](kernels.md)
- [Publish to Cloudflare R2](publish.md)
- [Installation](installation.md)
- [API](api.md)
- [Operations](operations.md)
- [Troubleshooting](troubleshooting.md)
