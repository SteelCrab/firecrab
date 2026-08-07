# M2 guest boot matrix

Unit tests do not prove that a guest can boot and reach the network.
The scheduled M2 jobs perform that full check.

## Test layers

| Layer | Purpose |
| --- | --- |
| Pull request jobs | Fast Rust, frontend, docs, and installer checks |
| Installer distro jobs | Package and dependency checks in containers |
| M2 boot jobs | Real KVM guest boot and network checks |

Container installer jobs do not boot a microVM.

## GitHub-hosted matrix

Scheduled and manual workflows test all supported image aliases.

| Guest image | Hosts |
| --- | --- |
| `alpine-3.24` | Ubuntu 22.04 and 24.04 |
| `ubuntu-26.04` | Ubuntu 22.04 and 24.04 |
| `rocky-9` | Ubuntu 22.04 and 24.04 |

Each matrix cell performs these steps:

1. Check `/dev/kvm`.
2. Install firecrab and the selected image.
3. Create a MicroNetwork.
4. Create and start a VM.
5. Wait for `running`.
6. Ping the guest.
7. Stop and delete the VM.

The workflow is `.github/workflows/ci.yml`.
The guest check is `scripts/ci-m2-guest-boot.sh`.

## Self-hosted runners

Set the repository variable `ENABLE_M2_SELF_HOSTED` to `true` to enable these jobs.
The workflow expects matching Linux, architecture, KVM, and distribution labels.

Self-hosted jobs cover Debian, Fedora, Arch, openSUSE, and an ARM64 Alpine case.
They are skipped when the variable is not enabled.

## Run locally

Install the required images first.
Do not run the whole installer through `sudo`.

```sh
./install.sh --with-ubuntu-image --with-rocky-image
scripts/ci-m2-guest-boot.sh alpine-3.24
scripts/ci-m2-guest-boot.sh ubuntu-26.04
scripts/ci-m2-guest-boot.sh rocky-9
```

The script creates its own MicroNetwork.
It fails if KVM, the helper, the image, or guest networking is not ready.

## Keep the matrix current

Add every new `default_specs()` alias to the workflow matrix.
Add the matching install option when the image is not built by default.
