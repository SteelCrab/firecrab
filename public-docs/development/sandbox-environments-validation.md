# Environment runtime validation — 2026-10-26

This record separates actual MicroVM evidence from Linux process fixtures.
Unexecuted checks remain pending. The database below is an explicitly scoped
SQLite JSON/TCP fixture on port 5432. It is not PostgreSQL.

## Prepared image

The existing OCI import API imported an owned Docker archive through a read-only
registry bound to the same QA manager's loopback interface. No saved registry
connection changed and the image was not published remotely.

| Identity | Recorded value |
| --- | --- |
| OCI manifest | `sha256:b1ce3cfdddcd8a653c4886d321a217bc304e4fdd8f8da2b53eb187c65acf071c` |
| Installed alias | `127.0.0.1-37687-firecrab-qa-seed-414-sha256-b1ce3cfdddcd` |
| Rootfs SHA256 | `ab04a910694d744377d27556afa787fd2bab2523db68c44d363e638662eaed78` |
| Rootfs bytes | `135266304` |
| Fixture source SHA256 | `b103e38a8239aed1eeb139d09e3333a90fbaeb185e838fdee1ab358de9942e43` |
| Initial canonical seed SHA256 | `cbcf0a2e4d8b712314490ec9d08466f3ef25f2a082907f941de03aac3d4f36b1` |

The prepared userland is Alpine 3.24.1. It contains Python, OpenRC, OpenSSH,
distribution tools and the trusted static UID/lock helper. Its selected kernel
artifact came from the existing OCI import workflow. Kernel image naming is
not used as evidence of the userland or init system.

## First environment

| Resource | Identity |
| --- | --- |
| Project | `7d07506d-3024-4b44-85aa-638098ebb3b7` |
| Environment | `a39192a2-5015-4bd6-8a1a-f7921551047f` |
| Generation | `cafb4e06-2d29-4f78-95da-439b7afc3778` |
| Network | `db54e339-ce6a-4028-9c15-de4936144e1b` |
| Dedicated subnet | `172.30.218.0/24` |
| Debug VM | `199cf8a6-6bd1-41cc-b25a-b9623c1c1d13` |
| Test VM | `2fa6dae0-b731-4839-ac32-43773cd289b6` |
| Backend VM | `e8845180-3b2b-4661-ac2d-f48345df623f` |
| DB VM | `66c25964-f7f2-4ad2-9953-9c2f9f26cfba` |

Each role reserves one CPU, 256 MiB RAM and a 1 GiB disk. All roles use
Persistent Workspace with no TTL. Backend depends on DB. Debug and Test depend
on Backend. Only Debug/Test → Backend:8080 and Backend → DB:5432 are declared.
The request, response, full manifest, security groups, leases and runtime IDs
were exported before operator probes. Project and environment creation each
used one PUT. No uncertain creation request was repeated.

### First failure and operator correction

DB started first and its port accepted connections. The agent capability check
failed because SSH port 22 refused connections. The existing OCI bootstrap
started the image application immediately but deferred even preinstalled SSH
until optional package downloads completed. Those downloads could not succeed
on the isolated network. The initial failure and console logs were retained.

An explicitly authorized operator correction started only the existing trusted
`/etc/firecrab/services.d/sshd` in the new DB VM. No package, key, image pin or
operator setting was changed manually. This correction is separate from
automatic provisioning acceptance.

After the correction, pinned-key SSH returned protocol 1, `init=openrc` and
`execution=true`. A real command returned stdout and stderr separately with
exit 7. `/proc/1/comm` was `init`, `/proc/1/cmdline` was `/sbin/init`, and
`rc-status -a` showed active Firecrab agent, OCI and console services.
The static helper probe returned exit 0 in the actual guest. This validates a
prepared Alpine/OpenRC OCI MicroVM; it does not validate the separate catalog
Alpine image, distroless, Docker or nspawn modes.

A separate actual guest helper invocation ran a file-free child as UID/GID
64555 with no extra supplementary groups and `NoNewPrivs: 1`. Explicit stdin
survived, stdout/stderr remained separate and exit 7 was preserved. Soft and
hard nofile limits were both 1024. This operator probe did not create a model
task or provider invocation.

The source fix starts an existing SSH binary before optional downloads and
does not start a second live SSH supervisor afterward. Native OCI writable VM
disks also refresh their managed bootstrap during specialization. The original
image pin and native PID1 configuration remain intact.

### Measurements before API recovery

The DB's initial canonical seed and fixture source checksums matched the pinned
manifest. DB → Internet IPv4 TCP 443 and DB → manager API TCP 5523 timed out.
The control connection to DB loopback TCP 5432 succeeded. A global IPv6 attempt
failed with `Network unreachable`; this measures missing routing, not a peer
link-local firewall drop. A link-local IPv6 address existed on guest eth0.

Automatic maintenance observed DB readiness and started Backend afterward.
An initial predeployment snapshot had DB and Backend running while Debug and
Test were created. Backend encountered the same legacy SSH delay.
All four VM, security group and network reservations were retained in a full
snapshot before restart. No explicit reconcile request was sent during this
capture.

While deployment was pending, the old Backend package worker eventually started
SSH. The old controller then started Debug and Test. The first GET after API
replacement already contained their running runtime identities. Both guests
still had the old bootstrap hash and zero early-SSH markers. They are not
evidence of successful first startup under the new code.

The running Backend connected to DB TCP 5432 and read the same seed. Backend
connections to DB TCP 22 and Internet TCP 443 timed out after two seconds.
Backend also attempted DB's actual link-local IPv6 address on TCP 22 and timed
out. The DB console separately confirmed SSH listeners on both `0.0.0.0` and
`::`. The exact DB and Backend bridge security-group chains were exported.
No guest address or firewall rule was changed to perform these probes.

Environment export returned 200 with an empty task list. DELETE with
`authorizeWorkspaceDeletion:false` returned 400 and retained the environment.
This proves the explicit Workspace deletion guard for an environment without
tasks; it does not prove required task artifact export. Final deletion was not
performed.

### Bootstrap upgrade and dirty-page failure

The operator replaced only the API binary. Manifest, generation, network and
all VM/security-group reservation identities stayed unchanged. The approved
existing Backend was stopped and started once. It then reached authenticated
SSH readiness without manual service correction. Its managed bootstrap hash
was `6182770a100c59cb9a093a9dc2ea47d557f47d5585797beb978904b83f9a685c`
with three early-SSH markers. Native PID1 and the image pin remained unchanged.

A later approved Debug upgrade wrote a unique file through the operator console
immediately before raw VM stop. The write and its checksum succeeded. After
restart the new bootstrap and SSH worked, but the file was absent. The original
failure is retained. The VM still had one disk generation; source inspection
confirmed reuse of existing disks. The existing raw VM stop terminated the VMM
without flushing guest dirty pages. Test's further lifecycle was held at this
point. This is not recorded as successful Workspace retention.

The source correction requires bounded, runtime-pinned guest filesystem sync
before managed Persistent Workspace stop. Both Environment and ordinary VM stop
reuse this guard. Admission stays blocked until the VM's Stopping claim is
persisted. Unconfirmed sync retains the VM. Persistent stage/final flush and
execution checkpoints also sync acknowledged guest state.

After deployment, four new role-specific marker files were written immediately
before normal Environment Stop. No operator `sync` or deliberate flush delay was
used. The last write completed less than one second before Stop returned. Stop
and Resume each ran once. All four marker contents and SHA256 values survived.
Generation, manifest, image pins, network, VM and security-group IDs stayed the
same. All runtimes changed. Every role reached authenticated SSH readiness using
the managed bootstrap, including Test without a manual service start.

The evidence is `actual_microvm_environment_stop_resume`, with four fresh
markers verified. The earlier raw-stop dirty-page failure remains a separate
negative result. This proves acknowledged filesystem retention through normal
stop/resume; it does not promise RAM retention or unacknowledged writes after
power loss. The legacy environment was then stopped normally for resource
capacity. Its disks, project, manifest and results remain retained.

### Four-role packet matrix

All four roles later reached Ready. Test retained its original runtime: its old
optional package worker eventually started SSH. No further lifecycle action was
used to obtain the following actual guest-to-guest measurements.

| Origin | Destination | Result |
| --- | --- | --- |
| Debug, Test | Backend TCP 8080 | Connected; HTTP seed read succeeded |
| Backend | DB TCP 5432 | Connected; fixture seed read succeeded |
| Debug, Test | DB TCP 5432 | Timed out |
| Debug → Test, Test → Debug | SSH TCP 22 | Timed out |
| Backend | DB SSH TCP 22 | Timed out |
| DB | Backend TCP 8080 | Timed out |
| Each of four roles | Internet IPv4 TCP 443 | Timed out |
| Debug, Test, Backend | DB link-local IPv6 SSH TCP 22 | Timed out |
| DB itself | Its same link-local IPv6 SSH TCP 22 | Connected; positive listener control |

All 13 IPv4 observations matched the manifest. Three IPv6 peer attempts were
denied while the DB's own IPv6 listener control succeeded. The initial seed
checksums read through Debug HTTP, Test HTTP and Backend DB access all matched
the immutable seed checksum. No guest address or policy was changed for these
measurements. These operator probes do not create a model/provider task.
The legacy network gateway still forwarded external DNS. Therefore these
measurements prove the listed TCP/IPv6 peer policy only; they do not prove a
complete external-DNS block.

## Linux process fixtures

The production shell executed in a disposable Linux container with distribution
UID/lock tools disabled. The static helper, UID/no-new-privileges, separate
streams and exit, hard/soft nofile 1024, output overflow, cancellation, recovery,
Workspace hydrate/overlay/delete/private retention and final flush passed.
The OCI startup worker also passed both preinstalled-SSH-before-packages and
missing-SSH-after-packages cases without a duplicate live service.

These containers used at most one CPU and 512 MiB RAM. Their outputs identify
them as Linux process fixtures with `microvmValidated=false`. They do not pass
MicroVM mode, provider or network acceptance.

## Private DNS process fixture

A separate Linux fixture ran the QA dnsmasq 2.91 binary in two owned disposable
network namespaces. It did not create a MicroVM or change the QA helper. The
binary SHA256 was
`8a307feb5fa212ffa5f0d93cbb7015dedc82be296328957fb5c67d3c4539741b`.
A local counting UDP/TCP upstream supplied synthetic documentation addresses;
no external DNS server was queried.

Per-interface and per-address `auth-server` prototypes blocked private DHCP.
They were rejected. Two separate resolver processes preserved DHCP Offer/ACK
for both networks. The legacy resolver first cached seven external RR types.
The private resolver then handled every QTYPE from 0 through 65535 over IPv4
UDP/TCP and IPv6 UDP/TCP: 262144 questions, zero external answers, zero extra
upstream queries. Another 184 known-type/recursion checks and 12 internal
service/DHCP-name checks passed. The upstream count stayed 12 across all private
queries, rose to 13 for a new legacy name, then stayed 13 for its cached repeat.

The full result SHA256 was
`4739c4e90754b6b0c3fc04e9a29a617bf397b05202e535d42d49b5925654ed91`.
Both fixture containers and networks were removed by exact owned IDs. This is
`dnsmasq_2_91_linux_fixture` with `microvmValidated=false`.

The product keeps existing network/Environment DNS posture as `legacy_forward`.
New managed environments record `private_only` outside their immutable input
manifest. A new capability operation prevents an old helper from silently
ignoring this policy. Private DNS has separate PID/config/lease/host files and
cache. It uses `no-resolv`, `no-poll`, `no-hosts` and `local=/#/`. DHCP and local
`.internal` names remain available. Readiness probes the VM's own DHCP FQDN.
Matching legacy resolver inputs and process identity permit adoption after a
helper restart without clearing its cache.

Private VM short names and `.internal` names follow the current IPAM
reservations, rather than a guest's dynamic DHCP lease lifetime. The helper
publishes them through the separate `dnsmasq-private-vms.hosts` input. A service
route refresh does not replace that file. A newer reservation snapshot or a
removed private network removes its names; a stale lease revision cannot
replace a newer checkpoint when the network set changes. Legacy DNS inputs and
forwarding behavior remain unchanged. The DNS-only regression uses real
dnsmasq on an owned high loopback port; it does not validate a live MicroVM.

Private networks share their local DNS name view within the private group.
This is not per-environment DNS name confidentiality. Each environment's
injected role/service discovery stays scoped to its captured reference and
allowed ports. REST resource permissions and IPv4/IPv6 traffic policy remain
separate. The host input exception permits DNS only to a VM's own gateway;
other gateway/manager DNS destinations are denied. IPv4-only managed policy
also drops IPv6 at the VM's Layer 2 boundary. Actual new strict-environment
DNS, direct-gateway bypass and cross-environment probes remain pending.

## Pending checks

Two-environment isolation and independent writes remain pending.
The first API restart preserved all resource identities during incomplete
readiness. Interruption during actual resource creation remains a separate
pending scenario. Prepared distroless, Docker and nspawn MicroVM runs must
each retain their own actual evidence. New strict-DNS environments must be
validated after the operator-controlled helper upgrade.
