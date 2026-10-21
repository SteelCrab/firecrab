# Firecrab documentation

Firecrab manages Firecracker microVMs on one host.
Linux runs the services directly; macOS and Windows use managed Debian.

## Get started

1. [Install Firecrab](getting-started/installation.md) for your platform.
2. Open the [dashboard](guides/dashboard.md) at `http://127.0.0.1:5523/`.
3. Create a network, install an image, then create and start a VM.

| Platform | Setup |
| --- | --- |
| Linux host or remote CLI | [Installation](getting-started/installation.md) |
| Apple silicon Mac | [microManager on macOS](getting-started/macos.md) |
| Windows with WSL2 | [microManager on Windows](getting-started/windows.md) |

## Use and operate

| Task | Guide |
| --- | --- |
| Create VMs, use the console, and connect with SSH | [Dashboard](guides/dashboard.md) |
| Configure and manage an app | [MicroService](guides/microservices.md) |
| Configure subnets, internet access, and IPv6 | [Networking](guides/networking.md) |
| Choose a disk location and back up VM data | [Storage](guides/storage.md) |
| Set management VM CPU, RAM, disk, and networking | [microManager resources](guides/micromanager-resources.md) |
| Install and register VM templates | [Images](guides/images.md) |
| Import a container image | [OCI images](guides/oci.md) |
| Install or change a guest kernel | [Kernels](guides/kernels.md) |
| Configure services, upgrade, or uninstall | [Operations](guides/operations.md) |
| Route domains and paths across apps | [Pipe ingress and tunnel](guides/pipe.md) |
| Diagnose a failure | [Troubleshooting](guides/troubleshooting.md) |

## Reference

| Topic | Reference |
| --- | --- |
| Resource names and terms | [Core concepts](reference/concepts.md) |
| Components, VM startup, and recovery | [Architecture](reference/architecture.md) |
| Commands and host profiles | [CLI](reference/cli.md) |
| REST and WebSocket contracts | [API](reference/api.md) |
| Guest init, environment, metrics, and SSH | [Guest runtime](reference/guest-runtime.md) |
| OCI stages, caches, and archive safety | [OCI import pipeline](reference/oci-import.md) |
| Managed Debian release inputs and migration | [microManager manifest](reference/micromanager-manifest.md) |

## Develop and validate

| Task | Document |
| --- | --- |
| Set up a checkout | [Contributing](../CONTRIBUTING.md#run-from-source) |
| Build Linux services on macOS or Windows | [microManager development](development/micromanager.md) |
| Build image packages | [Image builds](development/image-builds.md) |
| Publish the image catalog | [Cloudflare R2 publishing](development/publishing.md) |
| Run checks and runtime E2E | [CI](development/ci.md) |
| Choose QA scripts and report results | [QA](development/qa.md) |
| Run the complete checklist | [English](development/testing.md) · [Korean](development/testing.ko.md) |
| Test macOS repair | [Repair QA](development/macos-repair.md) |
| Review past platform results | [Validation history](development/validation-history.md) |

## Keep documentation short

- Put setup in `getting-started/`, tasks in `guides/`, contracts in `reference/`, and contributor work in `development/`.
- Explain a behavior once and link to it from other pages.
- Use short English sentences, with one sentence per source line.
- Keep the English and Korean test cases aligned.
- Keep planning notes and bug logs outside this tree.
- Run `python3 scripts/check-doc-links.py` from the repository root after edits.
