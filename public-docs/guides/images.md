# Images

An M2Image is a kernel and rootfs template for new VM disks.
Only installed images can create VMs.
Use the dashboard **Images** screen for inventory, OCI import, and MicroRegistry.

## Install

1. Open **Images → MicroRegistry**.
2. Select a package matching the host architecture.
3. Download and install it; wait for success before creating a VM.

Current release aliases are `alpine-3.24.1`, `ubuntu-26.04`, and `rocky-9.8` for x86_64 and ARM64.
ARM64 kernels use the distro PE32+ `Image`; x86_64 uses ELF `vmlinux`.
Kernels must match the host architecture.

The default package source is `https://registry.firecrab.dev`.
Set `FIRECRAB_IMAGE_BASE_URL` for a mirror or `none` to disable remote installs.
Image artifacts live below `FIRECRAB_IMAGE_ROOT` and are verified before use.
Deleting an installed image retains its staged package.

For container references, follow [OCI import](oci.md).
For kernel inspection and pairing, use [Kernels](kernels.md).

## Build a pulled image

Images downloaded through [Pipe Registry](pipe-registry.md) appear in **M2Image**
with **Build required** status. Select **⋯ → Build M2Image** on the source row.
The **Registry** column shows **Docker Hub**, **GHCR**, **ECR**, or **Harbor**
before and after a build. Hover over the registry name to see its full address.
Images without a retained source record show **—**.
Image names show the repository and tag. Digest references show the first 12
digest characters. Hover over the name to see the full reference or template alias.
The build uses the downloaded digest and reuses verified cached blobs.
Wait for **Installed** before creating a VM from the generated alias.
Running builds resume status polling after a page reload. Failed builds keep
their source and log so they can be retried.

## Register

Register an installed custom image to keep a reusable package in this host's catalog.

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/microregistry/register \
  -H 'Content-Type: application/json' \
  -d '{"alias":"nginx-1.27","version":"1"}'
curl -fsS http://127.0.0.1:5523/api/microregistry/register/nginx-1.27
```

Wait for `succeeded`.
Registration stores a local archive, checksum, and SQLite catalog row.
It survives API restarts and remains visible during public-catalog outages.
It publishes nothing remotely.
After template deletion, install from the local row to restore its verified package.
See [MicroRegistry contracts](../reference/api.md#microregistry) for collisions and cleanup.

## Bootstrap

MicroBoot builds an image in a temporary VM through the API.
It has no dashboard panel and needs no Docker or host chroot.

| Method | Route | Effect |
| --- | --- | --- |
| POST | `/api/images/{alias}/bootstrap` | Start |
| GET | `/api/images/bootstrap` | Active job or `null` |
| GET | `/api/images/bootstrap/{bootstrapId}` | Progress |
| DELETE | `/api/images/bootstrap/{bootstrapId}` | Cancel and remove builder |

Only one bootstrap can run at a time.
The builder stops before its disk is packaged and is removed after success, failure, or cancellation.
Rocky bootstrap downloads the pinned Container-Base archive without an installed Rocky template.

## Related

- [Build image packages](../development/image-builds.md)
- [Publish the catalog](../development/publishing.md)
- [Guest runtime](../reference/guest-runtime.md)
- [Troubleshooting](troubleshooting.md)
