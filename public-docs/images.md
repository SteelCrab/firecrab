# Images

An M2Image contains a kernel and root filesystem.
It is the source template for new VM disks.

Supported aliases are `alpine-3.24`, `ubuntu-26.04`, and `rocky-9`.

## Build

Build all supported images on an x86_64 Linux host.

```sh
./scripts/build-m2images.sh
```

Build one alias when needed.

```sh
./scripts/build-m2images.sh --alias alpine-3.24
```

Docker is used for Alpine and Rocky.
Ubuntu also uses a temporary privileged chroot.

Rocky guests include `dnf` so dashboard package actions work.
Rebuild `rocky-9` after pulling rootfs script fixes before testing dnf.

## Output

```text
dist/m2images/
  alpine-3.24.tar.zst
  ubuntu-26.04.tar.zst
  rocky-9.tar.zst
  SHA256SUMS
```

Verify packages before publishing them.

```sh
sha256sum -c dist/m2images/SHA256SUMS
tar --list --zstd --file dist/m2images/alpine-3.24.tar.zst
```

## Publish

Serve packages at `<base-url>/<alias>.tar.zst`.
By default the API uses the public MicroRegistry. Set
`FIRECRAB_IMAGE_BASE_URL` to use another package base URL, or set it to
`none` to disable remote installs.

The API downloads and validates a package first.
It installs the staged package in a separate step.

Deleting an installed image does not delete its staged package.

## Bootstrap

The dashboard can build a supported image in a temporary builder VM.
This path does not need Docker or a host chroot.

The builder downloads distribution files and creates an ext4 rootfs.
firecrab stops the builder before reading its disk.

Only one bootstrap job can run at a time.
The builder is removed after success, failure, or cancellation.

Rocky bootstrap needs an installed Rocky builder image.

## Install

Only installed images appear in the VM create form.
Image files live below `FIRECRAB_IMAGE_ROOT`.

The API validates paths and artifacts before use.
Restart the API after replacing files outside the API workflow.

## Add an alias

Update these parts together:

- Image build script
- Package output
- `default_specs()` in `firecrab-api/src/templates.rs`
- CI boot matrix in `.github/workflows/ci.yml`

## Related

- [Installation](installation.md)
- [API](api.md)
- [Operations](operations.md)
- [Troubleshooting](troubleshooting.md)
