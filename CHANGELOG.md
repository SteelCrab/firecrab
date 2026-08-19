# Changelog

All notable changes to firecrab are documented in this file.
Sections are **Added**, **Changed**, **Deprecated**, **Fixed**, and **Improved**; version headings match the Cargo workspace version.

## Releases

| Version | Date | Work |
| --- | --- | --- |
| [Unreleased](#unreleased) | — | [#145], [#147], [#151], [#152], [#158], [#165] |
| [0.1.1](#011---2026-08-17) | 2026-08-17 | [#141], [#142], [#143] |
| [0.1.0](#010---2026-08-16) | 2026-08-16 | First public release |

## [Unreleased]

Entries land here as work merges, and move under the next version heading when that release is cut.

### Added

- OCI import pairs a digest-pinned MicroRegistry kernel, cached at `.oci/kernel/<arch>/` ([#145]).
- `FIRECRAB_OCI_KERNEL_PATH` names a host copy of that kernel ([#145]).
- [`public-docs/oci.md`](public-docs/oci.md) gains a contents table and an import architecture diagram ([#145]).
- `firecrab-cli` workspace crate ships `firecrab doctor`/`info`/`status` ([#152], [#152-comment]).
- [`public-docs/firecrab-cli.md`](public-docs/firecrab-cli.md) documents the CLI's architecture and all three subcommands ([#152]).
- Left nav rail gets solid SVG icons matched to each feature (MicroVM, Networks, Storage, Images, Host), replacing Unicode glyphs ([#165]).

### Changed

- An unreachable MicroRegistry falls back to an installed catalog kernel ([#145]).
- Published pages may run to 300 lines, up from 170 ([#145]).
- Default `FIRECRAB_BIND_ADDR` moves from `127.0.0.1:3000` to `127.0.0.1:5523` — a dedicated
  4-digit port instead of a common dev port ([#147], [#147-comment]).
- README (all locales) and `CONTRIBUTING.md` point at port `5523`, not `3000` ([#147]).
- `public-docs/` API examples and check lists reference port `5523`, not `3000` ([#147]).
- `install.sh` no longer preinstalls a guest image — `--no-images`, `--with-images`,
  `--with-ubuntu-image`, and `--with-rocky-image` are removed along with everything only
  reachable through them; a guest image now comes from OCI import or the dashboard Images
  page, and guest-image handling moves to a future `firecrab` CLI instead of `install.sh`
  ([#151], [#151-comment]).
- CI's `vm-boot`/`vm-boot-self-hosted` import Ubuntu/Rocky via OCI import instead of the
  removed `install.sh` flags ([#151]).
- `scripts/firecrab-doctor.sh` and the `firecrab-doctor` binary alias are removed; use `firecrab doctor` ([#152]).
- OCI import and MicroBoot session logs auto-scroll to the latest line, matching `VmDetailModal` ([#165]).
- OCI import log box height is unified to `40vh`, up from the `28vh` cap ([#165]).

### Fixed

- A kernel download from a registry that stops answering times out ([#145]).
- `install.sh`'s API-readiness poll no longer prints a `curl: (7) Failed to
  connect` line on every retry before `firecrab-api` finishes binding its
  listener ([#158]).

## [0.1.1] - 2026-08-17

One `./install.sh` leaves a working host on SELinux, and image downloads work again.

### Added

- `firecrab-doctor` fails on a service confined to `init_t`, and prints the relabel command ([#142]).
- `firecrab-doctor` tests registry egress as the API service account ([#141]).
- `semanage` is installed on SELinux hosts, so the recorded file context applies ([2493c7d]).

### Changed

- `install.sh` labels binaries `bin_t` and relabels `$PREFIX/lib/firecrab`; uninstall removes the rule ([#142]).
- A package download takes its object key from the published catalog ([#143]).

### Deprecated

- None.

### Fixed

- Image downloads no longer answer `404` ([#143]).
- Both services stay out of `init_t`, where connect is denied and `nft` cannot exec ([#142]).
- `--doctor` survives a data directory private to the service account ([7eb6740]).
- A connect refused by local policy names SELinux, the unit sandbox, and firewall rules ([#141]).

### Improved

- Troubleshooting covers the SELinux failure end to end ([#142]).
- Transport errors carry their whole source chain ([#141]).

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

[Unreleased]: https://github.com/SteelCrab/firecrab/compare/v0.1.1...main
[0.1.1]: https://github.com/SteelCrab/firecrab/releases/tag/v0.1.1
[0.1.0]: https://github.com/SteelCrab/firecrab/releases/tag/v0.1.0
[#141]: https://github.com/SteelCrab/firecrab/issues/141
[#142]: https://github.com/SteelCrab/firecrab/issues/142
[#143]: https://github.com/SteelCrab/firecrab/issues/143
[#145]: https://github.com/SteelCrab/firecrab/pull/145
[#147]: https://github.com/SteelCrab/firecrab/issues/147
[#147-comment]: https://github.com/SteelCrab/firecrab/issues/147#issuecomment-5327667852
[#151]: https://github.com/SteelCrab/firecrab/pull/153
[#151-comment]: https://github.com/SteelCrab/firecrab/issues/151#issuecomment-5327863284
[#152]: https://github.com/SteelCrab/firecrab/issues/152
[#152-comment]: https://github.com/SteelCrab/firecrab/issues/152#issuecomment-5328210931
[#158]: https://github.com/SteelCrab/firecrab/issues/158
[#165]: https://github.com/SteelCrab/firecrab/pull/165
[2493c7d]: https://github.com/SteelCrab/firecrab/commit/2493c7d
[7eb6740]: https://github.com/SteelCrab/firecrab/commit/7eb6740
