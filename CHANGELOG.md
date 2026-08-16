# Changelog

All notable changes to firecrab are documented in this file.

The format uses the sections **Added**, **Changed**, **Deprecated**, **Fixed**,
and **Improved**. Version headings match the Cargo workspace version.

## [0.1.1] - 2026-08-17

Makes a single `./install.sh` leave a working host on SELinux distributions,
and unblocks every image download.

### Added

- `firecrab-doctor` fails when a firecrab service is confined to SELinux's
  `init_t` domain, and prints the relabel command for this host.
- `firecrab-doctor` tests registry egress as the API service account, so a
  connection the operator's own shell can make is never mistaken for one the
  service can.
- `semanage` is installed as a dependency on SELinux hosts, where the file
  context the installer records cannot be applied without it.

### Changed

- `install.sh` labels the installed binaries `bin_t` and relabels
  `$PREFIX/lib/firecrab`; uninstall removes the file-context rule again.
- A package download resolves its object key from the published catalog and
  falls back to the compiled manifest, instead of trusting the compiled key
  alone.

### Deprecated

- None.

### Fixed

- Image downloads no longer answer `404`: the compiled object keys had drifted
  from the published layout, which also blocked OCI import, since pairing needs
  an installed catalog kernel.
- Both services stay out of `init_t`, where an outbound connect is denied and
  the network helper cannot exec `nft` — every registry read failed with
  `Permission denied (os error 13)` while a shell reached the same registry.
- `--doctor` no longer aborts with an internal error on a normal install: a
  data directory private to the service account is reported, not entered.
- A registry connect refused by local policy says so, and names SELinux, the
  unit sandbox, and firewall rules, rather than only "error sending request".

### Improved

- Troubleshooting documents the SELinux failure end to end: `ps -eZ`,
  `audit2allow`, the relabel, and `setenforce 0` as a one-command confirmation.
- Transport errors carry their whole source chain, so DNS, TLS, and policy
  failures are distinguishable in one line.

## [0.1.0] - 2026-08-16

First public release of firecrab: a single-host Firecracker microVM manager
with an unprivileged API, a browser dashboard, and a capability-bounded
network helper.

### Added

- REST API and systemd-installable `firecrab-api` for creating, inspecting,
  editing stopped guests, starting, stopping, and deleting MicroVMs.
- Browser dashboard (English and Korean) with VM list, create flow, detail
  editing, serial console, logs, and host status.
- Browser serial console, including CJK IME composition.
- `firecrab-net-helper` over a versioned Unix-socket protocol so only the
  helper holds host network capabilities.
- Explicit MicroNetworks: isolated bridges, persistent IPv4, MAC, and
  hostname per VM, internet or isolated egress, and optional NAT uplink.
- MicroStorage pools plus the default host disk layout for VM artifacts.
- M2Image catalog install for Alpine, Ubuntu, and Rocky templates, each
  with that distro's own official kernel.
- OCI inspect and import: pull a registry image, merge layers, and register
  a bootable ext4 rootfs.
- MicroRegistry register of an already-installed local image into the
  in-memory catalog.
- Docker Hub login for OCI pulls (`GET`/`PUT`/`DELETE
  /api/microregistry/docker-hub`): one stored account per host, used by
  inspect, import, and the guest toolbox pull, so a shared egress IP no
  longer runs into the anonymous rate limit. The secret is write-only and
  is offered to Docker Hub alone.
- Runtime environment variables on stopped VMs, written into the guest
  service wrapper on the next start.
- Host and guest port forwards on the API and dashboard.
- `install.sh` host installer and `firecrab-doctor` health checks.
- Host install from a GitHub Release:
  `curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash`
- Host bundles for Linux `x86_64` and `aarch64` in both `gnu` (glibc) and
  `musl`: `firecrab-host-<arch>-<gnu|musl>.tar.gz`. `install.sh` detects
  the host libc (`--libc gnu|musl` to override).
- `install.sh --bin-dir` replaces installed binaries from a local directory
  without compiling on the host.
- Release-profile and cross-target GitHub Actions builds for
  `x86_64`/`aarch64` GNU and musl, plus `firecrab-host-*.tar.gz` bundles.

### Changed

- GitHub Releases publish only host bundles and `install.sh`. Release notes
  have no Binaries section and no API-only tarballs.
- `install.sh` downloads musl release binaries instead of building with
  rustup and npm on the host.
- Workspace version is `0.1.0` across the Rust crates.
- Dashboard package version is `0.1.0`.
- The pinned Rust toolchain is 1.96.0 (`rust-toolchain.toml`, workspace
  `rust-version`, and CI).
- Test assertions use stable `assert_matches!` so failures print `Debug`.

### Deprecated

- None.

### Fixed

- Piped `install.sh` (`curl | bash`) no longer crashes on unset
  `BASH_SOURCE`, and downloads the Firecracker installer when there is
  no git checkout.
- Piped `install.sh` asks for the sudo password on `/dev/tty` instead of
  telling the operator to run `sudo -v` first.
- Default `install.sh` does not install a guest image. Use the dashboard
  Images page, or pass `--with-images`.
- DHCP and DNS are allowed on each MicroNetwork bridge and through the
  common guest firewall profiles so guests can obtain a lease.
- Saving a running VM applies port-forward edits instead of dropping them.
- NAT uplink fallback in the dashboard is parenthesized so Vite accepts it.
- Image catalog architecture must be stated explicitly, including on
  arm64 installer doctor paths.
- Release smoke no longer waits for `firecrab-api` to exit, so a binary
  that actually starts cannot stall Publish GitHub Release for six hours.

### Improved

- README architecture diagrams cover the host, MicroNetwork, OCI import,
  and MicroVM boot path, including how to inspect guest PID 1.
- Public English docs under `public-docs/` describe install, networking,
  storage, images, OCI import, the API, and operations.
- CI runs fmt, clippy, workspace tests with Codecov, rustdoc, installer
  smoke (including Debian/Fedora/Arch/openSUSE deps), and the frontend
  lint/build on every pull request.
- Changelog validation is part of the documentation CI job so a release
  cannot drop a required section.

[0.1.0]: https://github.com/SteelCrab/firecrab/releases/tag/v0.1.0
