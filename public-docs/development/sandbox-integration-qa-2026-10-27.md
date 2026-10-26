# Sandbox integration QA — 2026-10-27

Implementation and direct QA remain in progress for #414 and Draft PR #520.
Canvas #496 waits for complete AI Sandbox implementation and direct QA
acceptance. Existing shared contracts do not authorize Canvas implementation.

## Source and validation boundary

The earlier actual runs below use Linux candidate `027cccc2`. API SHA-256 is
`6d149ba24e5b6de500b52b84105e2bcc6e520afc66457842cd9352c0e32d4812`.
Helper SHA-256 is
`8c4f1fbfc793b25889f5cfa2d814f70ff911fc2d188d781b594c33f19d527750`.
CLI SHA-256 is
`3a41daac99e7cb2ef07799ebedc702010458e2ac6235eb9f6b0e1697f1ac1afd`.

The later `eb0b50e2` merge includes main `e89d78b4` and its MicroService Deploy
queue. Sandbox, VM, network and helper execution paths are unchanged from the
live candidate. These live results are attributed to `027cccc2`, not represented
as execution of a different binary.

- Ordinary Linux API suite: **1,481 passed**, 18 ignored.
- Exact-candidate privileged guest contracts: **5 passed** with normal parallel
  execution. Prepared BusyBox process checks: **15 passed**.
- These process fixtures use disposable Linux containers. They are not MicroVM
  evidence and did not produce local coverage profiles.
- Frontend: 43 native tests and release WASM build passed. Actual Pipe AI reload
  verifies the restored account layout and ChatGPT icon.

The full API fixture requires a writable checkout, sufficient disk capacity and
an init process that reaps fakeroot's daemon children. Earlier read-only,
small-tmpfs and non-reaping container attempts retain their failures separately.
The complete successful run uses UID 1001, normal disposable overlay storage,
the required tools and Docker `--init`. Test assertions were retained.

CI at `eb0b50e2` passes Rust, rustdoc and the Release Build. That macOS hosted
runner did not acquire capacity and was cancelled before code execution.
Its changed-line coverage was **67.98%** against the unchanged **80%** target.
At `682ee479`, Rust CI passes. The official native coverage artifact hits
**6,068/6,986** added executable Rust lines (**86.86%**). This is the native
LCOV intersection, not the final merged Codecov result. Artifact `12669761490`
is 1,029,412 bytes with ZIP SHA-256
`de6202568229895b10adee0cb28a84492bc47c03d6abe19b2d6586428a23d967`.
Final merged Codecov patch coverage is **86.80%**, above the unchanged **80%**
target. CI run `40361397156` now has **18 successful jobs and one configured
skip**. Its single targeted Installer retry `121840115768` passes after the
upstream stable-kernel metadata becomes consistent. Release Build
`40361397132` has **25 successful jobs and one configured publication skip**.
These results apply to `682ee479`; the later private-DNS repair requires its
own exact-head CI run.
Follow-up Linux
Environment tests pass **15/15** and HTTP permission/lifecycle tests pass **6/6**.
They exercise exact resource reuse, retained Workspace deletion authority,
cleanup checkpoints across DB reopening, service-discovery scope and failed
readiness dependencies. Tunnel lifecycle tests pass **28/28**, CLI Sandbox tests
pass **25/25**, and MCP tests pass **17/17**. CLI/MCP groups overlap and must not
be summed as distinct tests. API/CLI test clippy passes with warnings denied;
format, documentation links and diff checks pass.

An early integration invocation selected two nonexistent Rust module filters and
ran zero tests. Those results do not count as validation. The corrected
`handlers::sandbox_tunnels::tests` and `mcp::tests` invocations produce the actual
28-test and 17-test results above. A separate image preflight lacked rustfmt;
formatting was checked with the installed native toolchain instead.

## Corrected defects

- Git 2.55 asynchronous maintenance created an advisory `objects/maintenance.lock`.
  Fixtures disable asynchronous maintenance. Snapshots ignore only a regular
  advisory lock without reading or copying it. Links, directories and unknown
  object entries remain rejected.
- Parallel attempts selected the same free GID. A bounded guest-global account
  lock now protects allocation, durable acknowledgement and cleanup. Payloads
  do not inherit its descriptor. Parallel creation and mixed cleanup pass.
- Legacy implicit-network migration used seven SQL parameters for eight
  placeholders. Migration now preserves `legacy_forward` explicitly.
- VM fixtures now persist their network membership. Production missing-network
  checks remain enforced.
- CLI/MCP artifact download now rejects a response for another task before any
  byte request. Metadata and the URL remain bound to the requested task UUID.
  Regression cases cover internally consistent foreign-task responses and
  foreign artifact owners. A distinct binary sentinel verifies that existing
  output content survives a refused overwrite.
- The offline OpenRC evidence checker now rejects root/missing GIDs and unequal
  real/effective user or group IDs. Its seven offline tests do not execute the
  guest payload or prove live guest acceptance.
- The old installed QA CLI lacked Pipe. A matching Linux CLI and verified
  ingress/cloudflared repair HTTP 503 to HTTP 200. See the
  [Pipe recovery record](sandbox-pipe-installation-preflight.md).

## Guarded deployment and capacity

Before replacing binaries, all five existing VMs were stopped and seven retained
tasks had no active or interrupted work. Full task pagination, executable
destination/hash checks and private backups protected the transition.

The initial post-start helper identity check failed. Root preserved that receipt,
verified the stable exact helper executable and private-DNS protocol-3 response,
then resumed API startup and compared all original references. No automatic
rollback or credential-store copy was used. All original connection IDs, task
event counts, Environment generations, VM/network/security-group IDs survived.

The QA system disk filled during staging. Only hash-verified duplicated public
QA executables and failed transfer fragments were removed. Binary staging moved
to the data disk. The same manager's data disk then grew from 16 to 24 GiB through
the official resource controller. After reboot, exact binary identities,
private-DNS support and all original metadata were verified again. The original
deployment receipt remains intact; a separate verification links its byte hash.

## Environment A: actual provisioning and controller restart

| Reference | Value |
| --- | --- |
| Project | `70b3dc57-c6e6-4033-8409-d00d7b406d30` |
| Environment | `4b9b2c97-bff6-4623-aa30-df7e9812c141` |
| Generation | `584299e1-aaf3-451b-aa0a-596575d7c02d` |
| Network | `13d96621-c6b9-4a31-8740-1d595aa330c0` |
| Subnet | `172.30.219.0/24`, private DNS, Internet disabled |
| Roles | Debug, Test, Backend and DB; each 1 CPU, 256 MiB RAM, 1 GiB disk |
| Retention | Persistent Workspace, no TTL |

The operator sent one project PUT, one Environment PUT, one API SIGKILL and one
explicit reconcile POST. API PID changed from 566 to 5008. Helper PID 533 remained
unchanged. A pidfd pinned the exact verified API process.

All four VM directories already existed immediately before and after the signal.
This does **not** demonstrate a crash with fewer than four allocated directories.
The interrupted environment had not completed readiness.

The first reconcile observation contained a transient DB agent-capability probe
failure while the Environment remained `provisioning`. The conservative harness
stopped and preserved that failure. Normal maintenance later reached `ready`.
An independent **GET-only** completion check verified every readiness layer,
the original generation and reserved IDs, exact expected resource additions,
unchanged old metadata and no duplicates. It sent no extra PUT, POST or signal.
The original harness is still recorded as incomplete; the later verifier is a
separate receipt.

All four guests have actual PID 1 `/sbin/init` and successful OpenRC service
status. Authenticated V1 capabilities report `init=openrc`, execution supported,
and Docker/nspawn unavailable. This is not yet an OpenRC task-payload or
limit/cancellation execution result.

Direct UI inspection selects the actual A project and expands its environment.
Debug, Test, Backend and DB each show ready agent/network/application status.
Backend HTTP `172.30.219.4:8080`, DB fixture `172.30.219.5:5432`, export-before-
cleanup and retained-lifetime labels are visible in Korean and English. A page
reload retains the project URL and language, then fetches the same ready
environment. No reconcile, stop, resume, export, delete or model operation was
sent during this inspection. Mobile acceptance remains open.

![Actual Environment A in Korean](../assets/sandbox-414-environment-a-ready-ko-2026-10-27.png)

![Actual Environment A in English after reload](../assets/sandbox-414-environment-a-ready-en-2026-10-27.png)

## Environment A: read-only network and seed checks

Actual checks: **237 passed**.

| Category | Evidence |
| --- | --- |
| TCP | 23: six live-listener controls, 13 same-Environment IPv4 checks, four IPv6 checks |
| IPv4 | Three allowed paths, six role-denied paths and four denied Internet paths |
| IPv6 | DB self-SSH positive control and three denied peer connections |
| DNS | 210: 192 external queries with no answers, eight own-DHCP resolutions, eight blocked legacy-gateway attempts and two legacy resolver controls |
| Seed | Four actual role reads match the same initial seed SHA |

The seed is a SQLite JSON/TCP fixture on port 5432, not PostgreSQL. Its canonical
initial hash is
`cbcf0a2e4d8b712314490ec9d08466f3ef25f2a082907f941de03aac3d4f36b1`.
No fixture writes were performed.

Role/runtime identities and legacy DNS PID 680, private DNS PID 4920, and their
configuration/hosts/lease hashes remained unchanged during these measurements.
The first inspection assumed JSON output from BusyBox `ip`; that fixture failure
is preserved. A read-only `/proc/net/if_inet6` decoder supplied the actual DB
link-local address without changing guest files or network configuration.

The host bridge has IPv6 disabled and no IPv6 DNS listener. Host-listener IPv6 DNS
bypass, cross-Environment traffic, independent writes and actual upstream packet
counts remain unverified. No listener or address was added to fabricate a control.

## Remaining gates

- B creation was rejected by automatic approval review before execution. The
  user then explicitly approved the exact B project, Environment and four-role
  VM allocation on this same private QA manager. One-shot creation reached Ready
  after 90.5 seconds. Initial seed and eight guest capabilities pass, but the
  initial A/B verdict fails because A's internal DNS names are unavailable.
  [The actual DNS record](sandbox-private-dns-qa-2026-10-27.md) preserves that
  failure, the guarded maintenance failure and the separate manual disposition.
  The repaired source `34a45b95` passes a fresh running A/B matrix: exact own
  addresses in 32 DNS replies, 96 denied cross-Environment IPv4 connections,
  384 external-name responses without answers, and six IPv6 peer denials with
  two positive controls. DNS names remain shared within the private resolver.
  Independent writes and OpenRC tasks still need their separate Root releases.
- Complete actual OpenRC argv/stdin/output/limits/cancellation and result export.
- Complete the remaining actual results/UI and public preview acceptance.
  [Retained-results follow-up](sandbox-results-ui-qa-2026-10-27.md) verifies
  actual Korean/English responsive screens and three artifact byte/hash checks
  after maintenance. A new non-code conversation and public preview remain open.
- Live Anthropic requires a manually registered workspace API key. Public Tunnel
  checks require the user's registered connection and explicit test domain.
- Keep PR #520 Draft while exact-head CI and direct acceptance remain incomplete. Keep
  Canvas implementation on hold.

Earlier actual evidence is retained in the
[recovery](ai-sandbox-recovery-qa.md),
[coordination](sandbox-coordination-qa.md),
[entrypoint](sandbox-entrypoints-validation.md),
[distroless](sandbox-distroless-validation.md) and
[execution-mode](sandbox-engine-validation-2026-10-26.md) records.
