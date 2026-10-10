# CI and runtime E2E

The [CI workflow](../.github/workflows/ci.yml) defines the automated checks.
It runs on pull requests, pushes to `main`, manual dispatch, and daily at 17:00 UTC.
Use the [QA work list](qa.md) and the [English](TEST.md) / [Korean](TEST.ko.md) checklists to record runtime results.
Passing build or unit checks does not establish MicroVM boot, SSH, or host port forwarding.

## Contents

- [Automated jobs](#automated-jobs)
- [Local checks](#local-checks)
- [Clippy warning gate](#clippy-warning-gate)
- [Windows runtime E2E](#windows-runtime-e2e)
- [macOS runtime E2E](#macos-runtime-e2e)
- [Results and evidence](#results-and-evidence)
- [Related](#related)

## Automated jobs

| Job | Runner | Checks |
| --- | --- | --- |
| `rust` | Ubuntu | fmt, workspace Clippy/tests/LLVM coverage, shared development deployment and guest QA contracts |
| `micromanager-macos-build` | Hosted macOS | Swift tests, signed helper and entitlement, CLI build/Clippy/tests, deployment contract, diagnostic JSON |
| `micromanager-windows-build` | Hosted Windows | CLI build/Clippy/tests, PowerShell 5.1 QA and Node browser-runner contracts, diagnostic JSON |
| `micromanager-pr-report` | Ubuntu, PR only | Summarizes both platform jobs for affected PRs; fork PRs get a job summary |
| `docs` | Ubuntu | Public links/language/length, changelog and release-note contracts, PR-report tests, rustdoc with warnings denied, 75% rustdoc coverage floor |
| `install` | Ubuntu 24.04 | Shell checks, install/service permissions, API QA, Chromium with real boot and IPv4/IPv6 SSH, nginx, Alpine/Ubuntu/Fedora, reinstall/uninstall |
| `install-distro` | Linux containers | Dependency installation on Debian 12, Fedora, Arch, and openSUSE; no systemd or guest boot |
| `frontend` | Ubuntu, Node.js 22 | `npm ci`, lint, TypeScript check and Vite build |

The Linux installer job requires working `/dev/kvm`; absence fails guest QA rather than skipping it.
Its Chromium step has a 30-minute budget, nginx has 25 minutes, and the public-image guest step has 90 minutes.
Guest references are `alpine:3.21`, `ubuntu:24.04`, and `fedora:42`; nginx uses `nginx:1.27-alpine`.
These checks fetch public images and first-boot packages, so retain repository/network failures in the result.

Hosted macOS and Windows jobs do not provide the nested virtualization needed for runtime E2E.
Their doctor JSON may report an unready host; the job checks the diagnostic contract and accepts exit 0 or 1.
Runtime tests require a separate capable host whose doctor reports `ready: true`.
The Linux installer job runs Playwright against the installed API using a local static BusyBox and OCI registry fixture.
There is no ARM64 KVM runtime job; native macOS/Windows browser runs remain manual.
Browser, nginx, and guest failures are retained without suppressing the other eligible runtime steps.
QA logs, JSON/JUnit results, and failure traces are uploaded as `linux-runtime-qa-<attempt>` for 14 days.
The [release workflow](../.github/workflows/release.yml) separately tests and packages release artifacts.

## Local checks

Run workspace checks on Linux with the pinned toolchain and the test dependencies, including `dnsmasq` and `fakeroot`:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
python3 scripts/test-micromanager-dev.py
python3 scripts/test-ci-qa-guest.py
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
```

For native Windows and macOS CLI changes, run `cargo clippy -p firecrab-cli --all-targets -- -D warnings` and `cargo test -p firecrab-cli --locked` on that OS.
Linux API/helper checks still run on Linux; Windows source deployment builds them inside managed Debian.
The link checker alone does not cover the docs job's rustdoc build or coverage floor.

## Clippy warning gate

Clippy runs with `-D warnings`, so a single warning fails the build.
There is no baseline file; the accepted number of warnings is zero.
`dead_code` counts, and an item only tests use belongs behind `#[cfg(test)]`.

Apply the machine-applicable suggestions, then review the diff:

```sh
cargo clippy --fix --workspace --all-targets
```

## Windows runtime E2E

Use a dedicated Windows host with WSL2 nested KVM and run PowerShell from the checkout root.
Build the native CLI, then test the checkout's API and net-helper explicitly:

```powershell
cargo build -p firecrab-cli --locked
.\scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli .\target\debug\firecrab.exe -Source . -WaitFactor 3
```

| Option | Meaning |
| --- | --- |
| `-Phase` | `gate`, `shell`, `api`, `nginx`, `guest`, `lifetime`, `browser`, or `all` (default) |
| `-Cli` | Windows CLI under test; defaults to `firecrab.exe` |
| `-Source` | Deploy the specified checkout with `service dev`; omission does not request source deployment |
| `-Release` | Build source API/helper with the release profile; requires `-Source` |
| `-LinuxCli` | Optional Linux binary for CLI checks inside Debian; otherwise uses its installed CLI |
| `-WaitFactor` | nginx/SSH guest wait multiplier, default 1; does not extend package-install or Playwright deadlines |
| `-ResultsDir` | Windows transcript/summary/browser archive destination; defaults to `target/qa/windows/<run-id>` |

`gate` checks doctor, installs or reuses microManager, optionally deploys source, checks service status, and requires the Windows localhost API.
An explicit `-Source` also runs the gate before an individual QA phase.
`shell` checks G6: root execution, literal arguments, active API service, and exit-code propagation from Windows.
The native CLI E2E also checks default stdin shells, the `run` alias, Unicode/empty/literal arguments, separate stdout/stderr, EOF, error recovery, and service survival:

```powershell
cargo test -p firecrab-cli --test windows_service_shell -- --ignored --test-threads=1
```

These eight tests require an installed management distribution; hosted CI compiles them and leaves them explicitly ignored. `FIRECRAB_WINDOWS_SHELL_CLI` optionally selects an existing native CLI.
`all` requires the gate and QA setup, then runs **shell → API → nginx → guest → lifetime → browser**, collecting every phase even after a test failure.
It exits 1 if any phase fails; individual phases retain their failure exit codes in `summary.json`.
The guest script also checks later OCI references after a failed reference and cleans each run's imported alias.
To exercise only Chromium, run it separately:

```powershell
.\scripts\ci-qa-windows-e2e.ps1 -Phase browser -Cli .\target\debug\firecrab.exe -Source .
```

API, nginx, and guest scripts execute inside `firecrab-debian`.
`lifetime` runs R1–R7/X7 in that distribution: VM units, external stop, fast restart, API restart/adoption, crash while the API is down, interrupted start, and unit cleanup.
nginx also requires HTTP 200 on Windows `127.0.0.1:18080`.
Chromium, Vite, and the local OCI registry execute inside WSL, with real guest boot enabled and `FIRECRAB_E2E_REUSE_SERVER=1`.
The browser phase installs Linux npm/Chromium/SSH dependencies and excludes Windows `node_modules` when staging the checkout.
This verifies the dashboard in Linux Chromium; native Windows API/forwarded HTTP checks are separate gates.

The runner stages QA under `/root/firecrab-qa/<run-id>`, normalizes shell CRLF endings, and keeps the distribution and development caches.
Each run retains `run.log`, `summary.json`, and any browser results in `browser-results.tar.gz` on Windows.
The summary reports `RUNNING` until completion and records unrun phases as `WARNING` after a failed prerequisite.
Resource cleanup does not uninstall QA dependencies or undo manual lab configuration changes.
The [Windows guide](micromanager-windows.md#known-limitation) explains why stock pinned-release runtime results differ from source results.

## macOS runtime E2E

Use a native M3-or-later Mac with nested virtualization and the signed helper.
`scripts/ci-qa-macos-e2e.sh` provides `gate`, `shell`, `repair`, `browser`, `api`, `nginx`, `guest`, `lifetime`, and `all` phases.
The gate requires isolated `FIRECRAB_INSTALL_DIR` and `FIRECRAB_MICROMANAGER_HOME` paths.
For source services, deploy with `service dev` and use the browser command in the [E2E guide](../firecrab-e2e/README.md).
Chromium runs on macOS; the registry and workload IPv6 SSH use the Debian management VM through SSH.
Missing required management SSH configuration is a failure.
The script defaults `FIRECRAB_QA_WAIT_FACTOR` to 3 for nginx/SSH waits.
The [repair phase](micromanager-repair-qa.md) checks G7/G7a on an idle installed
runtime, including missing registration, SSH/API failure advice, recovery, and
file preservation. `all` runs it after the shell phase and refreshes management
SSH settings afterward. Python guard/restoration tests run on hosted Linux CI;
native runtime evidence remains a separate Mac check.

## Results and evidence

Record the checkout revision, dirty/source-build state, OS/architecture, virtualization layers, phase/options, exit code, and logs.
Use `PASS`, `FAILED`, or `WARNING`; record every skip and its reason.
The complete browser suite currently expects **16 passed, 1 existing explicit skip** when guest boot is enabled.
The skipped failed-register case has no dashboard/API failure trigger; it remains outside browser coverage.
Save Playwright traces/screenshots on failure and guest console output before deleting failed guests.
Runtime runners require guest boot and the existing managed API; contradictory import-only configuration fails before browser execution.
SSH QA fails early only when both package-failure and missing-sshd terminal markers are present; a still-running package installer remains subject to the wait budget.
Verify QA resource removal and closed forwarded ports, then restore any manually changed DNS or management SSH settings.

The [Windows source validation snapshot](qa.md#windows-source-validation-2026-10-03) records the measured lab results and remaining failures.
Local checks and manual E2E results do not establish that a GitHub Actions run or a stock release passed.

## Related

- [Contributing](../CONTRIBUTING.md)
- [Browser E2E](../firecrab-e2e/README.md)
- [QA work list](qa.md)
- [Operations](operations.md)
- [Troubleshooting](troubleshooting.md)
