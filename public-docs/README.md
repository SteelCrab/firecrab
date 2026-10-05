# firecrab technical documentation

firecrab runs Firecracker microVMs on one Linux host.
This directory is the only public technical documentation.

Guides use short English prose, and the test checklist is available in English and Korean.
Each page covers one topic.

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
| Managed Debian VM on macOS | [microManager on macOS](micromanager-macos.md) |
| Managed Debian VM on Windows | [microManager on Windows](micromanager-windows.md) |
| microManager settings and automatic idle shutdown | [Settings and Sleepy](micromanager-settings.md) |
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
| Linux, macOS, and Windows QA list | [QA work list](qa.md) |
| Complete test checklist | [English](TEST.md) · [Korean](TEST.ko.md) |
| CI jobs, runtime E2E, and evidence | [CI and runtime E2E](ci.md) |

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

- Write guides in clear English; keep the two test checklists aligned in English and Korean.
- Put one sentence on each source line.
- Keep guides short; the API and complete test checklists may be longer when their coverage requires it.
- Keep section order consistent: title, body, Related.
- Use short paragraphs and relative Markdown links.
- Prefer symbolic links for aliases instead of copy files.
- Keep project planning notes and bug logs out of this tree; maintain published test checklists here.

Private project notes stay in local `docs/` and are not published.

Run the documentation check before a commit.

```sh
python3 scripts/check-doc-links.py
```
