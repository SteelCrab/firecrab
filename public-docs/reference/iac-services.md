# IaC service inventory through v0.4.0

This is the coverage contract for [#548](https://github.com/SteelCrab/firecrab/issues/548).
The baseline is tag `v0.4.0`, commit `d426b52ff04223aa0ba9c289fc492448cbb2cb86`.
It is a service baseline, not a release deadline.
The [machine-readable inventory](../../packaging/iac-v0.4.0.json) records dependencies and baseline support.
The [IaC contract](iac.md) defines shared lifecycle and Terraform integration decisions.

## Support status

`partial` means the native Stack manages only part of that service.
`experimental` means the REST Terraform example, including its separate runtime actions.
`missing` means no baseline declarative workflow exists for that row.
These statuses describe v0.4.0 and do not claim the target implementation is complete.
Every row requires both workflows before #548 can close.

| Resource | Scope | Required configuration and behavior | Source |
| --- | --- | --- | --- |
| `kernel` | host | Pinned version, architecture and content digest; installed state | [Contract](../guides/kernels.md) |
| `image` | host | Catalog/package/OCI source, immutable boot inputs, alias and installed state | [Contract](../guides/images.md) |
| `storage_root` | host | Registered path/device and capacity metadata; unregister without formatting or erasing borrowed data | [Contract](../guides/storage.md) |
| `volume` | host | Name, size, filesystem identity and explicit data-retention policy | [Contract](../guides/volumes.md) |
| `network` | host | IPv4/IPv6, uplink, internet access and DNS mode supported by the baseline API | [Contract](../guides/networking.md) |
| `security_group` | host | Name, network and ordered ingress/egress rules | [Contract](../guides/security-groups.md) |
| `shell` | host | Named scripts and immutable revision/content identities; assignment does not rerun scripts on refresh | [Contract](../../firecrab-api/src/handlers/shells.rs) |
| `vm` | host | CPU, RAM, disk, env, egress, shell pins, port forwards and desired running/stopped state | [Contract](../reference/api.md) |
| `volume_attachment` | host | Exact volume/VM references and guest mount path; inactive-VM attachment rules | [Contract](../guides/volumes.md) |
| `pool` | host | Pool spec, capacity and lifecycle policy; member VMs and leases stay controller-owned | [Contract](../../firecrab-api-types/src/pool.rs) |
| `schedule` | host | Start/stop action, one-time or cron timing, time zone and enabled state | [Contract](../guides/schedules.md) |
| `secret` | host | Metadata, write-only value input and rotation version; retain or explicitly delete the exact owned secret | [Contract](../guides/secrets.md) |
| `secret_connection` | host | External secret-store connection and mapping configuration; do not replay one-shot transfers | [Contract](../guides/secrets.md) |
| `auth` | host | AWS/GitHub/Firecrab account connection metadata and write-only credential input or existing verified reference | [Contract](../guides/pipe-auth.md) |
| `registry` | host | Registry/account/endpoint and write-only login or shared Auth reference | [Contract](../guides/pipe-registry.md) |
| `service` | host | Complete MicroService spec, immutable image, bindings, placement, health and desired running/stopped state | [Contract](../guides/microservices.md) |
| `service_auto_deploy` | host | Tag watch policy, enabled/paused state and explicit image-writer delegation | [Contract](../guides/deploy.md) |
| `service_otlp` | host | Exporter endpoint, enabled state, headers through secret references and telemetry policy | [Contract](../guides/otlp.md) |
| `service_deploy_token` | host | Token scope, expiry, rotation version and write-only delivery; creation is not repeated on refresh | [Contract](../guides/ci-deploy.md) |
| `ingress` | host | Listen settings, domain/path routes, app references and desired unit state; preserve other owners' routes | [Contract](../guides/pipe.md) |
| `quick_tunnel` | host | Saved target and desired connector state; generated URL is an observed output | [Contract](../guides/pipe.md) |
| `named_tunnel` | host | Zone/domain/route binding, credential input and connector state within the existing Cloudflare integration | [Contract](../guides/public-addresses.md) |
| `firecrab_tunnel` | host | Saved fixed-address target, account/token reference, connector state and explicit remote retirement policy | [Contract](../guides/firecrab-tunnel.md) |
| `database_connection` | host | Provider, endpoint/database, verified TLS and write-only login; manage the saved connection, not an external RDS/Supabase instance | [Contract](../guides/pipe-databases.md) |
| `database_binding` | host | Service/database association, role and access policy; retain external database data | [Contract](../../firecrab-api/src/handlers/pipe_database_bindings.rs) |
| `database_events` | host | Saved trigger/change-delivery spec and enabled state; event acknowledgements/replay are explicit operations | [Contract](../../firecrab-api/src/handlers/pipe_database_events.rs) |
| `database_rabbitmq` | host | External or managed RabbitMQ delivery configuration with credential references | [Contract](../../firecrab-api/src/handlers/pipe_database_events.rs) |
| `database_migration` | host | Versioned migration definition and checksum; explicit apply and no automatic replay/rollback of SQL | [Contract](../../firecrab-api/src/handlers/pipe_database_maintenance.rs) |
| `storage_connection` | host | Existing S3 bucket/region/prefix and credential reference; disconnection must retain bucket contents | [Contract](../guides/pipe-storage.md) |
| `queue_engine` | host | RabbitMQ engine spec, persistent storage and desired running/stopped state | [Contract](../guides/queues.md) |
| `queue` | host | Queue name, durability/type and settings; explicit message-data retention/deletion policy | [Contract](../guides/queues.md) |
| `ai_connection` | host | Provider/account/endpoint settings and noninteractive credentials or existing OAuth identity reference | [Contract](../guides/pipe-ai.md) |
| `ai_model` | host | Project-scoped connection/model defaults and verification requirements | [Contract](../guides/pipe-ai.md) |
| `sandbox_project` | host | Source identity, allowed files/skills, model defaults, coordination and runtime references | [Contract](../guides/ai-sandbox.md) |
| `sandbox_access` | host | Principal/resource/permission grants with exact host/project ownership | [Contract](../guides/sandbox-access.md) |
| `sandbox_environment` | host | Pinned role manifests, network policy, readiness, desired running/stopped state, TTL and workspace retention | [Contract](../guides/sandbox-environments.md) |
| `sandbox_tunnel` | host | Saved preview target, immutable execution reference, successor policy and connector state | [Contract](../../firecrab-api/src/handlers/sandbox_tunnels.rs) |
| `cluster_host` | controller | Enrollment endpoint, authenticated host UUID, labels/capacity configuration and draining state; heartbeat is observed, not fabricated | [Contract](../guides/multihost.md) |
| `cluster_placement` | controller | Placement spec, labels, host-qualified dependencies, exact recorded owner and desired VM state | [Contract](../guides/multihost.md) |
| `cluster_service` | controller | Controller service spec, pinned credentials, per-host targets, replicas, external-state declarations and desired state | [Contract](../../firecrab-api-types/src/cluster_services.rs) |
| `stack_source` | host | Repository/ref/path, polling/sync mode, stop permissions and pause state; never create two writers for one stack | [Contract](../guides/stack-git-source.md) |
| `management_settings` | management | microManager CPU/RAM/disk/network, autostart, port and Sleepy settings through a native management adapter | [Contract](../guides/micromanager-settings.md) |
| `host_configuration` | management | API/helper service configuration, mTLS file references and controller/worker configuration through a native management adapter | [Contract](../guides/operations.md) |

## Lifecycle by resource

These are target rules; stage 2 must supply any missing observation, update or safe-removal APIs.
Import uses the exact identity below without recreating the resource.
Read-only references never transfer ownership.

| Resource | Identity | Update or replacement | Ready when | Removal policy |
| --- | --- | --- | --- | --- |
| kernel | version and architecture | New content/version is a new immutable installation | Verified installed digest and architecture | Remove only owned unused artifact; retain borrowed installations |
| image | alias, version and boot hashes | New immutable version; alias changes never mutate admitted VM pins | Verified boot artifacts available for target architecture | Retain versions referenced by workloads |
| storage_root | host-qualified storage ID | Metadata in place; path/device change requires explicit replacement | Registered accessible root and validated capacity | Unregister only when unused; never format or erase borrowed data |
| volume | host-qualified volume UUID | Follow baseline immutability; size/filesystem replacement needs data consent | Verified on-disk identity; no conflicting writer | Retain data artifact and discoverable identity unless deletion authorized |
| network | host-qualified network UUID | Internet/DNS changes follow API; address-family/subnet/uplink change plans replacement when immutable | Bridge, lease service, firewall and requested DNS configured | Delete only when all owned dependents are removed and no external users remain |
| security_group | network-qualified UUID | Rules/name in place; network change requires replacement | Rules validated and applied to intended network | Reject removal while referenced; do not weaken another owner's VM policy |
| shell | shell UUID and revision hash | Create a new immutable revision; explicit VM revision reassignment | Content checksum and revision available | Retain revisions still referenced; no command execution on refresh |
| vm | host-qualified VM UUID | Supported edits in place; template/network/storage changes follow replacement policy | Confirmed declared runtime state plus guest/network readiness | Stop own VM; retain root data by default and detach retained data volumes |
| volume_attachment | volume UUID and VM UUID | Mount/target changes detach then attach under explicit stop permission | Exact exclusive attachment and required guest mount on next start | Detach and preserve volume contents |
| pool | host-qualified pool UUID | Follow mutable pool fields; incompatible template changes plan replacement | Desired pool capacity and member health; controller owns leases | Release only parent-owned members; borrowed images/networks remain |
| schedule | host-qualified schedule UUID | Update timing/zone/action/enabled state in place with generation checks | Stored validated schedule with next-run observation | Disable and delete schedule; preserve VM and run history policy |
| secret | host-qualified secret UUID | Metadata in place; write-only value rotation requires changed version marker | Saved version available; dependent rotation outcome reported | Retain by default; explicit deletion blocked while referenced |
| secret_connection | connection UUID | Settings in place or replacement if identity changes; never replay transfer plan | Saved provider connection with validated access | Disconnect only; preserve source and destination secret values |
| auth | provider and verified account identity | Credential rotation in place; account switch is explicit replacement | Existing provider verification succeeds | Disconnect only when no dependent references; preserve external account |
| registry | canonical registry and account | Rotate login in place; endpoint/account change is explicit replacement | Authenticated registry access or valid shared Auth reference | Remove saved login only; keep images and external repository |
| service | host-qualified service UUID | Supported updates through Deploy; preserve stable route/name and current release until health | Accepted release healthy and routed, or confirmed stopped | Stop/drain own replicas then remove config; preserve external application data |
| service_auto_deploy | parent service UUID | Policy/pause fields in place; image writer explicitly delegated | Saved watcher policy with ownership and pause status | Disable watcher; preserve active release |
| service_otlp | parent service UUID | Exporter policy/credential-version changes in place | Saved validated exporter configuration; delivery failure observable | Disable exporter; preserve application runtime |
| service_deploy_token | service UUID and token UUID | Scope/expiry/rotation changes revoke and replace token explicitly | Token issued and delivered securely without plaintext readback | Revoke exact token; preserve service and unrelated tokens |
| ingress | host and route/listener ownership key | Apply only owned route/listener generations; conflict on shared owner changes | Matching Nginx generation and unit readiness | Remove owned routes only; keep borrowed listeners and routes |
| quick_tunnel | saved tunnel UUID | Target edits in place if supported; replacement otherwise | Saved connector ready with observed generated URL | Stop/delete exact connector; keep upstream resources |
| named_tunnel | zone, tunnel and domain binding | Saved binding in place or explicit identity replacement | Verified existing integration's connector/DNS status | Disconnect owned integration objects only; no whole-zone deletion |
| firecrab_tunnel | account and fixed-address identity | Retarget with existing token/protocol rules | Connector and fixed-address verification reported separately | Stop local connector; remote retirement requires explicit policy |
| database_connection | connection UUID | Credential/config replacement through existing API with version checks | Verified connection/TLS and declared access capability | Disconnect config; retain database, rows and user-owned backups |
| database_binding | binding UUID and service/database identities | Role/access changes follow existing API; recreate generated account only explicitly | Binding provisioning and required access verified | Remove only binding-owned account/credentials according to policy; retain DB data |
| database_events | database-qualified trigger UUID | Pause/configuration changes through controller; identity changes replace | Trigger and delivery configuration active or confirmed paused | Detach owned triggers; preserve source rows and undelivered-event evidence |
| database_rabbitmq | host singleton or managed-engine identity | Connection metadata in place; engine replacement requires data policy | Verified RabbitMQ transport and configured delivery engine | Disconnect external broker; retain managed broker data unless authorized |
| database_migration | database UUID, version and checksum | Immutable migration record; changed checksum/version is explicit new operation | Exact recorded execution outcome; no replay on refresh | Retain execution history and schema; no implicit reverse SQL |
| storage_connection | connection UUID | Bucket/prefix/account identity changes explicit; mutable metadata in place | Verified bucket access and credential reference | Disconnect only; never delete external bucket or objects |
| queue_engine | host singleton and owned VM identity | Follow engine spec; incompatible storage changes require replacement | Broker healthy and desired runtime confirmed | Stop engine; retain broker disk/messages unless explicitly authorized |
| queue | engine-qualified queue UUID | Follow broker immutability; incompatible arguments require replacement | Broker declares intended queue and credentials | Retain queue/messages by default; explicit destructive policy for deletion |
| ai_connection | provider/connection identity | Saved settings and credential version in place; account switch explicit | Existing provider/model capability verification | Disconnect connection only; preserve external account and saved conversations |
| ai_model | project UUID | Connection/model selection in place with verification | Selected model verified for required features | Remove explicit default without deleting provider connection |
| sandbox_project | project UUID | Source/allowlists/defaults in place with active-task conflict checks | Stored source/runtime grants resolve correctly | Reject active/uncertain work; preserve borrowed source trees and results |
| sandbox_access | principal and project/resource identity | Replace exact grant generation; recheck active operation authority | Saved policy matches authenticated identity | Revoke grant without deleting project or runtime |
| sandbox_environment | project and environment UUID | Immutable manifest; spec changes require new UUID; runtime state mutable | Per-role agent/network/application checks | Export/active-attempt checks; retain Workspace unless explicitly authorized |
| sandbox_tunnel | project and preview binding UUID | Immutable execution reference; explicit successor retargeting | Local connector and public verification tracked separately | Remove own connector; explicit remote retirement; keep tasks/artifacts |
| cluster_host | authenticated worker UUID | Labels/capacity/drain via authenticated registration/management; identity change replaces | Verified enrollment and fresh heartbeat | Drain/unregister only after owned placements clear; never stop unrelated host workloads |
| cluster_placement | controller and placement UUID | Follow immutable admitted spec; state operations use recorded worker | Worker receipt and confirmed VM state | Delete through recorded owner; retain tombstone and permitted data |
| cluster_service | controller and service UUID | Supported generations in place; existing state-mode restrictions remain | Accepted generation healthy on required target hosts | Stop exact generation/replicas; retain shared external application data |
| stack_source | stack registration UUID | Generation-checked source configuration; writer transfer explicit | Exact commit/blob validation and mode-specific observation | Remove source metadata after active work; keep bound resources |
| management_settings | local management installation identity | Atomic native settings update; activation waits for idle environment | Saved and activated native configuration; unsupported platform explicit | Release IaC ownership; never uninstall or reset a borrowed management VM |
| host_configuration | native host installation identity | Validated service configuration with explicit restart permission | API/helper/mTLS ready at intended endpoint | Release IaC ownership; preserve installation, PKI and workloads |

## Bindings and inherited resources

Bindings are managed fields or separate resources with explicit references.
Service Secret, Queue, storage and security-group bindings belong to the service specification.
Database bindings, volume attachments and access grants have separate lifecycle identities.
Pool members, service replicas, Sandbox-owned networks/VMs and cluster worker receipts remain owned by their parent controllers.
They must not become independently managed children in another workflow.

## Operations and application data

Persistent configuration remains in scope even when its API currently exposes only a one-shot command.
Adding a safe observe/apply contract is an implementation gap, not a reason to omit the service.
Interactive OAuth consent is completed by the user; IaC configures and references the resulting verified account.
IaC does not submit model prompts, lease pool members, replay events, edit database rows or synchronize S3 objects on refresh.
Logs, metrics, generated URLs, health probes, transcripts, task attempts, artifacts and heartbeats are observed results.
Database backup/restore, schema execution, Secret transfers and image builds are explicit versioned operations; an unchanged refresh must not repeat them.
Disk contents, queue messages, remote database rows, object contents and Sandbox workspaces are application data.
Destroying a connection or parent configuration does not authorize erasing borrowed or external data.
Resource-specific destructive consent and dependency checks remain required.

## Host boundaries

A `host` resource is applied to the selected API host, including managed Linux on macOS/Windows.
A `controller` resource uses existing authenticated cluster APIs and host-qualified references.
A `management` resource uses the local/native management layer because the guest API cannot configure its own outer VM or TLS listener.
The management adapter is a required implementation gap, not a requirement for manual per-service setup.
Installing the initial trusted Firecrab endpoint, obtaining external accounts and issuing CA trust are bootstrap prerequisites.
External platforms remain managed through their existing Firecrab integration boundaries.
For example, PipeStorage connects an existing bucket; it does not provision AWS S3 itself.

## Verification

Run `python3 scripts/check-iac-inventory.py` to check inventory integrity and references.
The inventory is complete only when all baseline public configuration families are represented.
Each implementation must add field/lifecycle tests and real runtime evidence for its row.
Fixtures alone cannot establish Linux/KVM, database, messaging, tunnel or controller/worker readiness.
