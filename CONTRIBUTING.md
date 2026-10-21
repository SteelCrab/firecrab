# Contributing to firecrab

Thanks for helping improve firecrab.

This guide covers how to set up a development environment, what to change where, and how we review pull requests. For product concepts and operator docs, start with [`public-docs/`](public-docs/README.md).

## A note from the maintainer

<p align="center">
  <img src="assets/icons/contributors.png" alt="Contributors" width="120" />
</p>

**Contributions are welcome.**  
We publish as much information as we can so anyone can join in. Small work counts too — typo fixes, minor bug reports, and similar help are all appreciated. Final review and merge are done by SteelCrab.

**Security and stability come first.**  
This project is complex and aims for features that fit enterprise environments. We care more about security and stability than shipping features for their own sake.

**Please file install failures as Issues.**  
If `install.sh` fails partway through, do not assume it is only your machine. Open an Issue when you can. Environment details, logs, and where it stopped already help a lot.

**Treat each other with respect.**  
Be courteous with other contributors. Prefer positive language and a light emoji over harsh or negative wording. 🙏

**Overlapping work is integrated together.**  
When several people work on similar features, SteelCrab will coordinate the merge so the result is a shared contribution.

**It is okay if maintenance pauses.**  
If life makes it hard to keep a PR going, the maintainer may pick up the work, polish it, and land it. We understand personal circumstances. Showing up and contributing at all is already a big help — a stalled commit or PR does not make the effort meaningless.

## What firecrab is

firecrab is a single-host microVM manager built on Firecracker.
It consists of an API, a network helper, a CLI, and a dashboard.
The API keeps host privileges minimal; the helper handles network operations that need them.

## Prerequisites

- **Common:** The repository's Rust toolchain, Node.js 22 or later, and npm
- **Linux:** Guest runtime tests require /dev/kvm and network tools.
- **macOS:** Uses microManager; runtime validation requires nested virtualization.
- **Windows:** Uses microManager in WSL2; runtime validation requires nested virtualization.

A full installation is unnecessary for unit tests and frontend builds.

## Run from source

Run these commands from the repository root.

**Linux:** Build the API and network helper, then run each command in a separate terminal.
VMs run in systemd units the helper starts, so the host needs systemd; rerun the helper script after rebuilding either binary.

```sh
cargo build -p firecrab-api -p firecrab-helper

# Terminal 1: network helper (copies both binaries to a root-owned directory)
./scripts/dev-net-helper.sh

# Terminal 2: API
cargo run -p firecrab-api

# Terminal 3: dashboard
npm run dev --prefix firecrab-frontend
```

**macOS:**

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service dev
```

For service commands and development options, see the
[macOS guide](public-docs/development/micromanager.md#macos).

**Windows PowerShell:**

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service dev
```

`service dev` installs microManager when missing, builds the local API and network
helper inside WSL2, and restarts them. On macOS and Windows it also starts the
source dashboard at `http://localhost:8080`; Node.js and npm are required.
The command verifies both the dashboard and its API connection before returning. See the
[Windows guide](public-docs/development/micromanager.md#windows)
for build, restore, and frontend options.

## Tests

Run workspace checks on Linux; use the [native CLI checks](public-docs/development/ci.md#local-checks) on Windows and macOS:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
python3 scripts/check-doc-links.py
```

Run browser E2E checks without guest boot:

```sh
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

Changes to VM start, stop, or the API's lifetime also need the VM lifetime checks (R1–R7), which restart the API on a real host:

```sh
scripts/ci-qa-lifetime.sh alpine:3.21                               # Linux, from the repository root
FIRECRAB_MICROMANAGER_HOME="$HOME/Library/Application Support/Firecrab/micromanager" \
  scripts/ci-qa-macos-e2e.sh lifetime                                # macOS
```

For platform scenarios, manual steps, expected results, and cleanup, see the [English TEST guide](public-docs/development/testing.md) or [Korean TEST guide](public-docs/development/testing.ko.md).
Windows source runtime checks use `.\scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli .\target\debug\firecrab.exe -Source .` from PowerShell.
`all` collects every test phase after the runtime gate and fails overall if any phase fails; use `-Phase browser` for only browser/guest boot.
The [E2E guide](firecrab-e2e/README.md) describes where browsers and fixtures run on each platform.

## Commits

Use a short subject that describes one change. For example:

```text
fix(api): …
docs: …
ci: …
```

## Pull requests

Keep each [pull request](https://github.com/SteelCrab/firecrab/pulls) focused. Explain what changed and why, link related issues, and report test results.
Record applicable TEST items as PASS, FAILED, or WARNING.

### Writing the PR body

Use the commit subject format for the title (`type(scope): description`). Write the body in English, in sections of short bullets, and drop any section that does not apply:

````markdown
## New Features

- Each VM's shim runs in its own systemd unit, so VMs survive API restarts

## Security

- The helper derives the unit name and uid/gid itself; nothing privileged comes from the request

## Fixes

- A requested stop exits the shim 0, so a normal stop leaves no failed unit

## Docs

- `public-docs/guides/troubleshooting.md`: VM units that outlive the API

## Validation

1. Test

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items
cargo test --workspace --locked
```

Notes: none

2. E2E

```sh
scripts/ci-qa-macos-e2e.sh browser
```

Notes: WARNING A8 skipped `a failed register job leaves no current catalog row`

3. QA

```sh
scripts/ci-qa-macos-e2e.sh api
scripts/ci-qa-macos-e2e.sh nginx
scripts/ci-qa-macos-e2e.sh guest
```

Notes: FAILED V8d `ssh root@172.168.0.2` never became ready on the macOS management VM

Related: #123, #336

One short paragraph on scope: what is deliberately left out, follow-up work, and the merge order of stacked PRs.
````

- Keep each bullet to one change a user or operator can see: a behavior, an API field, a command, or a file.
- Validation always has the three parts in this order: Test, E2E, QA. Each lists the exact commands that ran, then one `Notes:` line.
- Write `Notes: none` when every check passed. Otherwise note only the exceptions: a skipped or partial [TEST](public-docs/development/testing.md) item as WARNING and a failed one as FAILED, each with its ID and what happened.
- A part that did not run keeps its heading and says why in `Notes:`. Name the platform when it is not obvious from the commands (Linux, macOS microManager, Windows, GitHub CI).
- Link issues and related PRs on the `Related:` line. A stacked PR names the PR it is based on and the merge order.
- Do not add generated-by or tool attribution lines.

See [#216](https://github.com/SteelCrab/firecrab/pull/216) for an example.

## Issues

Report bugs and installation failures in an [issue](https://github.com/SteelCrab/firecrab/issues) with reproduction steps, environment details, and logs.
Prefer one problem per issue. Report sensitive security problems privately to the maintainers.

## CI

The [CI workflow](https://github.com/SteelCrab/firecrab/blob/main/.github/workflows/ci.yml) runs Rust, frontend, documentation, installer, and available automated scenario checks.
See [CI and runtime E2E](public-docs/development/ci.md) for exact job coverage, local checks, source/release options, and evidence requirements.

GitHub-hosted macOS and Windows CI cannot provide nested virtualization, so direct microVM runtime validation requires manual steps.
Depending on the contribution, contributors may post their manual test results in a PR comment.
If you have a better way to validate these changes, propose it in an issue so we can improve the process together.

## License

Contributions are covered by the project's [Apache License, Version 2.0](./LICENSE).
