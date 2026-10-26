# AI Sandbox environments

An environment is one immutable, single-host manifest. It owns a dedicated
IPv4 MicroNetwork, one MicroVM per role, and one security group per MicroVM.
It does not borrow existing VMs or own shared image templates.

Use `GET /api/ai-sandbox/projects/PROJECT/environment-images` to obtain complete
template pins. A pin includes the registered version and the kernel, rootfs,
initrd and boot-argument hashes. Changing an alias does not change an admitted
environment. Provisioning and dispatch reject mismatched image or resource IDs.

## Lifecycle

Environment endpoints are under
`/api/ai-sandbox/projects/PROJECT/environments`:

| Request | Behavior |
| --- | --- |
| `GET` | List this project's saved manifests. |
| `PUT /ID` | Reserve a client-generated identity and all resource UUIDs, then provision. |
| `GET /ID` | Read the manifest, ownership IDs, readiness and service discovery. |
| `POST /ID/reconcile` | Continue incomplete provisioning with the same IDs and observe readiness. |
| `POST /ID/stop` | Stop dedicated VMs and retain their root disks. |
| `POST /ID/resume` | Start the same VMs and recheck readiness. |
| `POST /ID/export` | Verify durable task results and artifact bytes. |
| `DELETE /ID` | Export first, then delete only recorded owned resources. |

Reading requires project read authority. Resource mutations require the
controller operator. Request bodies cannot select another owner or replace
resource IDs. A repeated PUT with the same manifest returns the same generation.
A changed manifest requires a new environment UUID. Deletion keeps a tombstone.

The controller persists all reserved UUIDs before creating a network, security
group or VM. Reconciliation uses the existing Runtime lifecycle and validates
the actual resource against the reservation. Partial failure or API restart
does not allocate replacement IDs. Incomplete provisioning and eligible TTL
cleanup are inspected every 30 seconds. Failed provisioning requires explicit
reconciliation. Environment management never retries a task command.

## Readiness and services

Each role exposes separate `agent`, `network` and `application` states. A running
VMM is not enough. Agent readiness probes the pinned, authenticated guest channel
and the selected execution engine/image. Network readiness requires its lease
and a successful host network recovery state. Application readiness checks the
TCP ports declared with `readiness: true`. Dependencies must be ready before a
dependent role starts. A dependency cycle is rejected before resources are reserved.

Declare services in `spec.roles[].services`. Discovery returns only the role's
current dedicated IPv4 address and declared ports. Literal argv tasks also receive
`FIRECRAB_SERVICE_ROLE_SERVICE_HOST` and `FIRECRAB_SERVICE_ROLE_SERVICE_PORT` for
services that their exact role may reach. These values never resolve services
from another environment. A task cannot override these injected values.

Templates must contain the services that their readiness declarations require,
or an operator must prepare them before dispatch. A missing application stays
pending. Missing guest execution support is reported as unsupported.

## Network policy

The manifest contains exact role-to-role TCP allows. A destination port must be
declared by the destination role. Provisioning creates matching ingress and
egress rules with the peer's exact `/32` lease. All other new connections are
denied. The dedicated network has no Internet egress and no IPv6 configuration.
Existing Runtime bridge and routed security-group enforcement blocks IPv6 frames.

For the four-role acceptance scenario, declare `backend/http:8080` and
`db/postgres:5432`, then allow only:

```json
[
  {"from":"debug","to":"backend","port":8080},
  {"from":"test","to":"backend","port":8080},
  {"from":"backend","to":"db","port":5432}
]
```

The policy denies Debug/Test → DB, DB → Internet, and other environments.
These are enforced policies. Deterministic policy tests do not replace the
separate live packet and IPv6 acceptance checks.

## Execution and retention

`agent_sandbox` runs Docker or systemd-nspawn inside the managed MicroVM.
The controller host never starts these task containers. Docker requires a
pre-installed `repository@sha256:digest` image. It receives an attempt-specific
name and ownership label, an unprivileged UID, no capabilities, no privilege
escalation, fixed resource limits, and only owned workspace/control mounts.
The Docker socket and host/provider credentials are not mounted. Its network is
the MicroVM's network, where the host security group enforces the role policy.

nspawn requires systemd and cgroup v2 in the managed guest. Install the exact
root archive at `/var/lib/firecrab-sandbox-images/DIGEST.tar`. Its SHA256 must match
`image: "sha256:DIGEST"`. Execution extracts a private copy and binds only owned
workspace and command files. Cleanup removes the private copy and preserves the
shared archive. Missing engines, images or required capabilities fail explicitly.

`persistent_workspace` executes directly as an unprivileged guest user.
Workspace code, user packages and configuration live on the dedicated root disk.
Attempt cleanup removes process controls and temporary users. It preserves the
workspace and prevents later UID reuse from exposing it. VM stop/resume and API
restart retain the same disk. Workspace manifests cannot use automatic TTL
deletion. Final deletion requires an explicit request body:

```json
{"authorizeWorkspaceDeletion":true}
```

Before the first model/file tool of a new Workspace attempt, the controller
hydrates retained allowed files into the isolated task copy. It records only
the host-validated allowed baseline. Subsequent staging applies explicit edits
and deletions. Excluded user packages and private configuration remain in the
guest and are never added to the deletion baseline.

Temporary environments may set `retention.ttlSeconds` between 60 and 604800.
Cleanup refuses active or uncertain attempts, missing export checkpoints, and
missing or changed artifact bytes. Export failure retains resources. Cleanup
never deletes shared images, borrowed VMs, unrelated networks or provider accounts.

## Validation limits

Contract tests cover immutable reservations, restart persistence, dependency
ordering, exact network allows, missing exports and retained ownership. Guest
shell fixtures cover bounded execution and workspace preservation. Real systemd,
OpenRC, distroless, Docker, nspawn and four-VM network runs need separate evidence.
The Canvas foundation gate is not accepted by fixture results alone.
