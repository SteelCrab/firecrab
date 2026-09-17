# firecrab technical documentation

firecrab runs Firecracker microVMs on one Linux host.
This directory is the only public technical documentation.

Every page is short English prose.
Every page covers one topic.

## Start here

1. Read [Architecture](architecture.md).
2. Follow [Installation](installation.md).
3. Create a [MicroNetwork](networking.md).
4. Create and start a VM with the [API](api.md) or [Dashboard](dashboard.md).

## Guides

| Topic | Document |
| --- | --- |
| Components and data flow | [Architecture](architecture.md) |
| Terms and resource model | [Core concepts](concepts.md) |
| Host setup | [Installation](installation.md) |
| Headless host CLI | [firecrab CLI](firecrab-cli.md) |
| Browser UI | [Dashboard](dashboard.md) |
| REST and WebSocket endpoints | [API](api.md) |
| Bridges, DHCP, NAT, and policy | [Networking](networking.md) |
| VM disk placement | [Storage](storage.md) |
| Kernel and rootfs templates | [Images](images.md) |
| Kernel lifecycle | [Kernel management](kernels.md) |
| Release upload to R2 | [Publish to Cloudflare R2](publish.md) |
| OCI inspect and import | [OCI images](oci.md) |
| Services and maintenance | [Operations](operations.md) |
| Failure checks | [Troubleshooting](troubleshooting.md) |
| Clippy warning regression check | [Clippy warning gate](ci.md) |

## Name aliases

These symbolic links keep short names stable.

| Alias | Target |
| --- | --- |
| [HOME.md](HOME.md) | [README.md](README.md) |
| [install.md](install.md) | [installation.md](installation.md) |
| [web.md](web.md) | [dashboard.md](dashboard.md) |
| [network.md](network.md) | [networking.md](networking.md) |
| [micro-network.md](micro-network.md) | [networking.md](networking.md) |
| [micro-storage.md](micro-storage.md) | [storage.md](storage.md) |
| [m2image.md](m2image.md) | [images.md](images.md) |
| [glossary.md](glossary.md) | [concepts.md](concepts.md) |

## Documentation rules

- Write in clear English only.
- Put one sentence on each source line.
- Keep each real file at 170 lines or fewer, except [API](api.md).
- Keep section order consistent: title, body, Related.
- Use short paragraphs and relative Markdown links.
- Prefer symbolic links for aliases instead of copy files.
- Keep tasks, tests, bugs, and design notes out of this tree.

Private project notes stay in local `docs/` and are not published.

Run the documentation check before a commit.

```sh
python3 scripts/check-doc-links.py
```
