# Image builds

Run from the repository root on a Linux host matching the target architecture.
For template installation, see [Images](../guides/images.md).

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

## Add an alias

For a new distribution family, add its manifest entry and builder, then update:

- Bootstrap guest script mapping
- CI boot matrix in `.github/workflows/ci.yml`

## Local host payload

Build API, helper, CLI, and dashboard together, then install them:

```sh
./scripts/ci-prepare-install-payload.sh
./install.sh --bin-dir target/release
```

The script requires the pinned Rust toolchain, Node.js, and npm.
For a manual build, compile `firecrab-api`, `firecrab-helper`, and `firecrab-cli` with `cargo build --release --locked`, then run `npm ci` and `npm run build` in `firecrab-frontend`.
Pass `--dashboard-dir firecrab-frontend/dist` to select that dashboard explicitly.

## Related

- [Publish to Cloudflare R2](publishing.md)
- [CI](ci.md)
