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
| Register hosts and place VMs through one controller | [Multi-host placement](guides/multihost.md) |
| Update or roll back a running app through Nginx | [Deploy](guides/deploy.md) |
| Configure service telemetry export | [OTLP export](guides/otlp.md) |
| Connect apps with internal RabbitMQ message queues | [Queue](guides/queues.md) |
| Validate a declarative YAML stack offline | [Stack validation](guides/stacks.md) |
| Register stack ownership and preview configuration changes | [Stack planning](guides/stack-planning.md) |
| Apply YAML stacks and inspect durable execution status | [Stack apply](guides/stack-apply.md) |
| Connect a GitHub manifest and detect configuration drift | [Git source checks](guides/stack-git-source.md) |
| Try Terraform against the public REST API | [Experimental Terraform example](../examples/terraform/basic/README.md) |
| Schedule MicroVM start and stop operations | [Schedules](guides/schedules.md) |
| Store secrets and transfer them to external stores | [Secrets and PipeSecrets](guides/secrets.md) |
| Configure subnets, internet access, and IPv6 | [Networking](guides/networking.md) |
| Restrict VM connections and inherit replica policies | [Security groups](guides/security-groups.md) |
| Choose a disk location and back up VM data | [Storage](guides/storage.md) |
| Keep application data when replacing a VM | [Persistent volumes](guides/volumes.md) |
| Set management VM CPU, RAM, disk, and networking | [microManager resources](guides/micromanager-resources.md) |
| Edit microManager settings and automatic idle shutdown | [Settings and Sleepy](guides/micromanager-settings.md) |
| Install and register VM templates | [Images](guides/images.md) |
| Import a container image | [OCI images](guides/oci.md) |
| Install or change a guest kernel | [Kernels](guides/kernels.md) |
| Configure services, upgrade, or uninstall | [Operations](guides/operations.md) |
| Route domains and paths across apps | [Pipe ingress and tunnel](guides/pipe.md) |
| Connect AWS and GitHub accounts to Pipe services | [Auth](guides/pipe-auth.md) |
| Select models, converse, and run isolated project tasks in a MicroVM | [AI Sandbox](guides/ai-sandbox.md) |
| Manage AI account connections and use shared CLI/MCP integration APIs | [Pipe AI](guides/pipe-ai.md) |
| Connect and browse an existing database | [Pipe databases](guides/pipe-databases.md) |
| Connect an existing Amazon S3 bucket and manage files | [PipeStorage](guides/pipe-storage.md) |
| Pull images from Docker Hub, GHCR or Amazon ECR | [Pipe Registry](guides/pipe-registry.md) |
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
| Validate AI models, isolated tasks, guest execution, and recovery | [AI Sandbox QA](development/ai-sandbox-qa.md) |
| Test macOS repair | [Repair QA](development/macos-repair.md) |
| Review past platform results | [Validation history](development/validation-history.md) |

## Keep documentation short

- Put setup in `getting-started/`, tasks in `guides/`, contracts in `reference/`, and contributor work in `development/`.
- Explain a behavior once and link to it from other pages.
- Use short English sentences, with one sentence per source line.
- Keep the English and Korean test cases aligned.
- Keep planning notes and bug logs outside this tree.
- Run `python3 scripts/check-doc-links.py` from the repository root after edits.
