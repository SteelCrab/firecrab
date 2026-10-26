# QA Pipe installation boundary and registration preflight

This is an operator preparation receipt for the existing #414 QA manager. It is
not an account-registration, engine-installation, service-start or public-preview
receipt. The investigation changed no remote file, service, VM, Environment,
network, account or token. Root performs any later installation after checking
the exact Linux artifacts. The user registers the connection directly.

## Actual failure and read-only receipt

The `#/pipe/firecrab-tunnel` page received `503 pipe_unavailable` because the
privileged helper selected an installed CLI without the `pipe` subcommand.
Its process started successfully; the error was not evidence of an unreachable
helper or an Nginx/cloudflared installation failure.

The source examined was integration `936339bb` and the matching entrypoint
sources. Read-only SSH to the existing `192.168.64.66` manager observed:

| Item | Actual result before CLI replacement |
| --- | --- |
| Manager architecture | `aarch64` |
| Helper | PID 4272, `/usr/local/lib/firecrab/firecrab-helper`, active |
| API | PID 62262, `/usr/local/lib/firecrab/firecrab-api`, active |
| Helper sibling CLI | `/usr/local/lib/firecrab/firecrab` absent |
| Selected fallback CLI | `/usr/local/bin/firecrab`, root:root, 0755, 7,924,136 bytes |
| Fallback format | ELF64, e_machine=183 (AArch64) |
| Fallback SHA-256 | `4344b8ab10cdd995d765fa9fd6f736c6069e3f8ae8bd7169c00ba2962cde2a78` |
| `firecrab --help` | Exit 0; commands doctor/info/status/update/vm/network/image/host/service, no Pipe |
| `firecrab pipe --help` | Exit 2; `error: unrecognized subcommand 'pipe'` |
| Manager `GET http://127.0.0.1:5523/api/pipe` | 503, fixed `pipe_unavailable` body |
| Manager TCP port 24314 | No listener; this is not its local API port |

The default ingress/cloudflared binaries, cloudflared marker, Pipe manifest and
ingress config were absent. Ingress, tunnel and address-updater units were
`not-found`, `inactive`, MainPID 0. `systemctl show` itself returned exit 0 for
these absent units. The helper/API units remained running throughout.

No provider DB, process environment block, connection-token file or private-key
content was read. The existing pinned management key was referenced only by
`ssh -i`. Public executable bytes were read only for ELF metadata and SHA-256.

## Metadata availability is independent of engines

`firecrab-helper/src/pipe.rs::cli()` looks for a protected root-owned sibling
`firecrab`, then `$PREFIX/bin/firecrab` (default `/usr/local/bin/firecrab`). It
does this for each request. With no sibling on this manager, an atomic replacement
of the correct fallback CLI can repair the command boundary without restarting
the API/helper or any VM. Do not install a CLI only at a different path and assume
the helper will discover it. Do not retain a stale protected sibling that wins
over the intended fallback.

For `Operation::Inspect`, the helper executes only `firecrab pipe inspect`.
`firecrab-cli/src/pipe/host.rs::snapshot()` reads optional configuration and live
unit metadata. Missing engines become `nginxAvailable:false` and
`cloudflaredAvailable:false`. Missing configuration becomes `config:null`.
Uninstalled systemd units report disabled/inactive metadata. Inspect does not
install an engine, start a unit, register a token or perform a remote heartbeat.

A compatible CLI plus working systemd inspection and an absent/valid manifest
must therefore return HTTP 200 metadata even with both engines absent. An invalid
manifest or a systemctl execution failure is a separate operational error. The
read-only receipt above predates CLI replacement, so it does not claim that the
live 200 check has already passed.

API changes `e793c357` and `d6982138` classify only the exact first Clap error line
as `501 pipe_cli_unsupported` with static guidance to update the CLI and helper
together. Unknown failures remain `503 pipe_unavailable`. Only request_id and a
static failure_code enter this API warning; raw helper stderr is not logged or
returned. The focused wire-mock REST tests cover old CLI stderr, unknown-canary
redaction in the response/log and successful missing-engine metadata. Their
Cargo execution is delegated to Root's existing integration cache.

## Exact engine paths and preparation order

These are the standard layout paths. Root must confirm any explicitly configured
PREFIX, FIRECRAB_LIBDIR, CONFDIR or FIRECRAB_INGRESS_BINARY override without
dumping unrelated environment variables or secrets.

| Component | Standard path and ownership |
| --- | --- |
| CLI selected by current helper | `/usr/local/bin/firecrab`, root:root 0755 |
| Private ingress executable | `/usr/local/lib/firecrab/firecrab-ingress`, root:root 0755 |
| Ingress notices | `/usr/local/lib/firecrab/ingress/{LICENSE.nginx,LICENSE.pcre2,LICENSE.musl,sources.json}`, root:root 0644 |
| Managed cloudflared | `/usr/local/lib/firecrab/pipe/cloudflared`, root:root 0755 |
| Public install marker | `/usr/local/lib/firecrab/pipe/cloudflared.json`, root:root 0644 |
| Generated manifest/config | `/etc/firecrab/pipe/{manifest.json,ingress.conf}` |
| Generated units | `/etc/systemd/system/firecrab-{ingress,tunnel,address-updater}.service` and per-connector units |

Pipe does not discover an arbitrary system `nginx` on PATH. Ingress comes from
the Firecrab host bundle, or an explicit protected `FIRECRAB_INGRESS_BINARY`
override. `scripts/build-ingress.sh OUTPUT_DIRECTORY` builds a static Linux
binary and includes the nginx, PCRE2 and musl notices. It requires a Linux musl
toolchain; a macOS Mach-O CLI or ingress file is never a manager artifact.
`packaging/ingress-sources.json` pins nginx 1.30.6 and PCRE2 10.49. The builder
verifies both source archives before building and rejects dynamic output.
Use a matching arm64 host release or Root's isolated Linux arm64 builder, with
two build jobs at most. Do not use the active Cargo cache for this investigation.

The separate `firecrab pipe install` requires ingress to exist first. It downloads
only the pinned cloudflared release, verifies its size and SHA-256, then writes
the managed binary and marker atomically. It does not configure or start the
traffic services. The current aarch64 manifest entry is:

- Version: `2026.10.0`.
- Public artifact: `https://github.com/cloudflare/cloudflared/releases/download/2026.10.0/cloudflared-linux-arm64`.
- SHA-256: `e6422b9d4f72d3194bc5a38676f13667c06666523217b842a877d72a80b5ac08`.

This is the repository's fixed pin, not a claim about the newest release.
Never substitute another cloudflared binary or hand-write a marker to make
`current()` pass. The installer compares the recorded asset/version and actual
binary hash. `cloudflaredAvailable` alone checks file presence; a readiness
preflight must also validate executable permissions, the pin/marker and version.

Root's staged sequence, after installation authorization, is:

1. Verify the recorded source SHA, compile target, byte hash, ELF class/machine
   and actual libc of the CLI before copying it. Run the existing
   `scripts/verify-release-binary.sh ARTIFACT aarch64 musl` or `gnu`, matching the
   recorded target. Do not select libc from the filename or build host.
2. Keep a private hash-checked backup of the current public executable. Replace
   only the selected CLI atomically with the verified root-owned candidate. Do
   not run the full host installer against the active QA environment.
3. Run that exact CLI's `pipe --help` and read-only `pipe inspect`. Check the API
   metadata GET separately. Both engine flags may still be false at this stage.
4. Install only the verified private ingress bundle and its notices. Validate
   its AArch64 ELF and `-V` output. Preserve the existing API/helper/VM state.
5. Run the matching CLI's `pipe install` as operator to install the pinned
   cloudflared and marker. This is preparation, not permission to publish.
6. Inspect again. Expect engine flags true, no token, no connector and no running
   ingress/updater before user registration. Record actual results and hashes.

Do not install a distro-wide nginx service or start a default public port 80
listener to satisfy this preflight. Managed ingress is rendered and controlled
through its own fixed unit when the approved connection is created.

## Before the user registers the test connection

Root must first record successful results for the exact deployed CLI, helper,
API and current static assets:

- Architecture/source/hash checks identify Linux AArch64 artifacts, not the
  previous native macOS build.
- `GET /api/pipe` returns 200 redacted metadata. An error page or cached 503 is
  not a successful account preflight.
- Both private engine paths are verified, present and executable. The pinned
  cloudflared install marker matches its bytes. No service start is needed to
  verify a binary version.
- The Firecrab Tunnel panel loads without browser/auth automation. The API's
  disabled engine state must not be confused with a helper transport failure.
- The two owned strict-DNS Environment seed/recovery checks and exported
  artifacts remain intact. Port 8080 is reserved for the separate bounded HTTP/
  WebSocket overlay after those checks pass.

The user then registers directly and supplies the test domain and saved
connection name. The agent never opens an authentication page, obtains or
reads the token, or substitutes another account. Before the later Connect action,
Root must confirm the exact owned Backend runtime/port and the authorized fixed
address. Connect can automatically install/start its private connector and
updater and begin public routing. It is not a metadata-only preflight.

Use [the bounded fixture recipe](sandbox-preview-qa-recipe.md) for that later
HTTP/WebSocket-only overlay. Record live public HTTP, WebSocket frames, local
stop and remote retirement separately. This installation receipt does not mark
any of them complete.
