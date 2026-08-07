# firecrab documentation

firecrab runs isolated Firecracker microVMs on one Linux host.
These documents explain how to install, use, and inspect it.

## Start here

1. Read the [architecture](10-overview/architecture.md).
2. Follow the [installation guide](20-guides/install.md).
3. Open the [dashboard](20-guides/web.md).
4. Create a [MicroNetwork](20-guides/explicit-micro-network.md).
5. Create and start a microVM.

## Core concepts

- [Architecture](10-overview/architecture.md)
- [Glossary](10-overview/glossary.md)
- [AWS comparison](10-overview/aws-mapping.md)

## Operations

- [Installation](20-guides/install.md)
- [Web dashboard](20-guides/web.md)
- [REST API](20-guides/api.md)
- [API errors](20-guides/api-error.md)
- [MicroNetwork](20-guides/explicit-micro-network.md)
- [MicroStorage](20-guides/micro-storage.md)
- [M2Image builder](20-guides/m2image-builder.md)
- [Network helper](20-guides/net-helper.md)
- [CI boot matrix](20-guides/m2-ci-boot-matrix.md)
- [Troubleshooting](20-guides/troubleshooting.md)

## Project records

The folders below contain implementation history.
Some older records are still written in Korean.

- [`30-tasks`](30-tasks/MOC-tasks.md) contains task notes.
- [`40-tests`](40-tests/MOC-tests.md) contains detailed test records.
- [`50-bugs`](50-bugs/MOC-bugs.md) contains bug investigations.
- [`superpowers`](superpowers/) contains design plans and specifications.
- [`90-appendix`](90-appendix/firecracker-manual/README.md) contains manual Firecracker notes.

## Contributing to the docs

Follow the [documentation style](00-meta/doc-conventions.md).
Use relative Markdown links.

Run the link check before committing.

```sh
python3 scripts/check-doc-links.py
```
