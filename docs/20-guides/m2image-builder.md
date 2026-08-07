# M2Image builder

An M2Image contains a kernel and root filesystem for Firecracker.
firecrab supports Alpine 3.24, Ubuntu 26.04, and Rocky Linux 9.

Only installed images can create VMs.

## Build all images

Run the builder on an x86_64 Linux host.
Docker is used for Alpine and Rocky.
Ubuntu also needs `sudo` for its temporary chroot.

```sh
./scripts/build-m2images.sh
```

Build one image when a full rebuild is not needed.

```sh
./scripts/build-m2images.sh --alias alpine-3.24
./scripts/build-m2images.sh --alias ubuntu-26.04
./scripts/build-m2images.sh --alias rocky-9
```

## Output

Packages are written below `dist/m2images`.

```text
dist/m2images/
  alpine-3.24.tar.zst
  ubuntu-26.04.tar.zst
  rocky-9.tar.zst
  SHA256SUMS
```

Verify the output before publishing it.

```sh
sha256sum -c dist/m2images/SHA256SUMS
tar --list --zstd --file dist/m2images/alpine-3.24.tar.zst
```

## Publish packages

Serve each package at this path:

```text
<base-url>/<alias>.tar.zst
```

Set the base URL on the API.

```sh
FIRECRAB_IMAGE_BASE_URL=https://images.example.invalid/firecrab \
cargo run -p firecrab-api
```

The dashboard first downloads and validates a package.
It then installs the staged package into `FIRECRAB_IMAGE_ROOT`.

Deleting an installed image does not delete its staged package.
Delete the staged package separately when it is no longer needed.

## Bootstrap in a builder VM

The dashboard can build a supported distribution inside a temporary microVM.
This path does not require Docker or host chroot access.

The builder VM downloads the distribution base files.
It installs packages and creates the final ext4 rootfs.

The builder VM is stopped before firecrab reads its disk.
This prevents a partially written filesystem from being packaged.

Only one bootstrap job can run at a time.
The API removes the builder VM after success, failure, or cancellation.

Rocky bootstrap requires an installed Rocky image as its builder.
Alpine and Ubuntu can use another installed supported image.

The result is staged below the image root.
Install it from the Images screen without a remote base URL.

## Add a new distribution

The web bootstrap list supports the known aliases only.
Add a new distribution through the build scripts and template registry first.

Update these parts together:

- Image build script
- Package output
- `default_specs()` in `firecrab-api/src/templates.rs`
- CI boot matrix

See [CI boot matrix](m2-ci-boot-matrix.md) for guest boot coverage.
