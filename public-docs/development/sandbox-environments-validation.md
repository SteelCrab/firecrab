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
execution checkpoints also sync acknowledged guest state. Deployment and actual
normal Environment stop/resume retention measurements remain pending.

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

## Pending checks

Two-environment isolation and independent writes remain pending.
The first API restart preserved all resource identities during incomplete
readiness. Interruption during actual resource creation remains a separate
pending scenario. Prepared distroless, Docker and nspawn MicroVM runs and retained
Workspace lifecycle checks must each retain their own actual evidence.
