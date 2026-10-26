# Docker, nspawn and Workspace QA recipe

Preparation is complete. Real mode execution remains pending the integrated
runtime deployment signal. Preparation creates no Environment, VM, engine,
registry listener or model task. It changes no existing QA resource or account.

## Public, offline inputs

The preparation host resolves Ubuntu 26.04 arm64 dependencies with a dedicated
APT configuration, empty package status, separate lists/cache/logs and
download-only mode. Its Debian package state is not used or changed. The only
repositories are the Ubuntu ports `resolute`, `resolute-updates` and
`resolute-security` main/universe suites.

The archive key is obtained from Ubuntu's public keyserver in a separate
keyring. Its fingerprint is checked against the [Ubuntu security documentation](https://documentation.ubuntu.com/security/software-integrity/image-verification/):
`F6ECB3762474EDA9D21B7022871920D1991BC93C`. APT checks the signed indexes. Each
downloaded package SHA-256 is then compared with `apt-cache show` metadata from
those indexes. Independent `gpgv` checks of all three saved InRelease files pass.
No package maintainer script is executed during preparation.

The 111 packages total 99,988,686 bytes. They include:

| Package | Version |
| --- | --- |
| docker.io, including client/daemon | `29.1.3-0ubuntu4.1` |
| containerd | `2.2.2-0ubuntu1.2` |
| runc | `1.4.0-0ubuntu1` |
| systemd-container | `259.9-0ubuntu0.1` |
| python3 | `3.14.3-0ubuntu2` |
| busybox-static | `1:1.37.0-7ubuntu1` |

Private QA preparation directory on the management host:
`/var/lib/firecrab/qa-engine-prep-414-7c766dda`.
`bundle.json` records every package version, architecture, filename and SHA-256.
Logs and manifests are mode 0600. The final offline archive includes all debs,
the public key, actual signed InRelease files, image assets and asset builder:

- `offline-engine-bundle-with-signatures.tar`
- Size: `103731200` bytes
- SHA-256: `5e972f4e6bbbd5ed5198a337d451eda56f33cac037809efd488361a4a698d1b2`

## Small content-addressed images

`scripts/qa-sandbox-engine-assets.py build` accepts the verified package ledger
and exact busybox-static deb. It checks the deb SHA-256 and Linux AArch64 ELF
payload. It writes a deterministic, root-owned filesystem with a static shell,
basic applets and regular account/OS metadata files. It creates no devices and
extracts no arbitrary archive paths onto the preparation host.

The same filesystem is used as the Docker layer and nspawn root archive.
Two independent builds produce identical image metadata and hashes.

| Input | Pin |
| --- | --- |
| BusyBox deb SHA-256 | `fcf50794faf920455895d7ef103d1c54b9ebf7f18d2e52982dbf3903b7769592` |
| Docker repository | `127.0.0.1:24144/sandbox414/busybox@sha256:6f9343062f60921e9e19ccfeef46159b3ba4e5184b4f7df8632449b55ef3d3ce` |
| nspawn image | `sha256:01a1ffd69933520827f23b86ca833c9c5b64737d9f90b089242ab91cc166d990` |
| nspawn archive size | `2017280` bytes |

Example build, on a host with `dpkg-deb`:

```sh
python3 scripts/qa-sandbox-engine-assets.py build \
  --bundle ./bundle.json \
  --deb './debs/busybox-static_1%3a1.37.0-7ubuntu1_arm64.deb' \
  --output ./images
```

`serve` implements only exact read-only Registry V2 routes and binds
`127.0.0.1`. Its port must match the pinned image origin. Stop it after the
operator's pull, before admitting a model task. It is not an engine API.

```sh
python3 scripts/qa-sandbox-engine-assets.py serve --root ./images --port 24144
```

Docker documents automatic loopback-registry treatment, but the actual pull is
still an acceptance check: [dockerd registry rules](https://docs.docker.com/reference/cli/dockerd/#insecure-registries).
Do not change daemon flags or guest egress to make the test pass. nspawn's
rootfs and privilege options follow its [command documentation](https://www.freedesktop.org/software/systemd/man/259/systemd-nspawn.html).

## Operator bootstrap after the deployment signal

1. Capture the deployed API/helper identity, saved connection/model metadata,
   shared template hashes and borrowed-resource inventory. Use only the existing
   saved connection. Fix a new project, Environment and role identity before
   provisioning. Keep the managed network and VM Internet policy isolated.
2. Use the approved immutable Ubuntu 26.04/systemd base. An agent Environment
   may report unsupported until its engine and exact image pin exist. Its
   reserved VM/resource ledger remains authoritative during this preparation.
3. Verify that VM/runtime ownership, SSH host identity and OS match. Copy the
   SHA-checked offline bundle only to that new owned VM. Simulate the local
   package transaction, then perform operator installation with networking
   disabled and `apt-get --no-download --no-install-recommends`. Install only
   bundled, verified packages. Record package versions and engine readiness.
4. Docker: start the operator's temporary loopback registry in the owned guest,
   pull the exact repository digest, inspect `RepoDigests`, then stop that
   registry. Do not expose the engine socket, registration credentials or
   operator helper controls to the model.
5. nspawn: copy the verified tar to
   `/var/lib/firecrab-sandbox-images/<sha256>.tar`. Check its SHA-256 and regular
   root-owned file status. Do not alter the shared host template or reuse a
   borrowed VM. Reconcile the Environment and inspect each readiness layer.
6. Docker and nspawn Environments run serially from the same approved engine
   preparation inputs. Any private prepared-template publication needs its own
   immutable identity; never replace the shared base alias or version.

## Required real evidence

Each task uses a unique nonce and retains project/task/attempt/runtime/
Environment generation references. Store complete tool input, stdout, stderr,
exit code, model result, test events and artifact digest. Metadata, fixture
success or a model statement cannot replace actual guest output.

| Case | Evidence and acceptance |
| --- | --- |
| Docker/nspawn command | Non-root UID, `/proc/self/status` NoNewPrivs=1, expected capability restrictions, owned workspace nonce, stdout/stderr and actual exit 7. Do not report an intentional exit 7 as a passed command. |
| Engine isolation | Actual task cannot access an engine socket or operator/root control files. Model does not choose root flags, engine configuration or another runtime. |
| Timeout | Bounded sleeping command ends by its real wall deadline. Inspect owned process/container termination; preserve the actual failure. |
| Cancellation | Cancel a running owned task, inspect cancellation/termination, then confirm no owned process/container remains. A request receipt alone is insufficient. |
| Export | Retain an actual output artifact, export it, and verify exact byte size and SHA-256 before cleanup. |
| Required-export failure | Reversible operator fault injection is limited to a precisely identified new QA artifact. Preserve original bytes separately. Export/cleanup must fail and retain the owned guest/resources. Restore the original artifact and verify export before final cleanup. Never change durable task flags or an account database to simulate success. |
| TTL | Use a temporary agent Environment with a bounded TTL. Observe cleanup after completed export, GET 404 for owned resources and preservation of shared templates/borrowed resources. Failed export must retain resources. |
| Workspace persistence | Task writes a nonce plus package/cache/configuration files excluded from result archives. Stop/resume, admit a new attempt on the new runtime and read those same guest files. Packages and configuration remain on disk. |
| Workspace final deletion | DELETE without authorization is rejected and data remains. Export results, explicitly authorize final deletion, then verify absence of every owned VM/network/security group. |

No real mode case above has been accepted by this preparation record. Record
individual unsupported or failed results without substituting another VM or
quietly relaxing policy. Public HTTPS/Tunnel publication and Canvas runtime
integration remain separate work.
