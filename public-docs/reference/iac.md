# IaC contract

This is the target contract for [#548](https://github.com/SteelCrab/firecrab/issues/548).
It covers every Firecrab service shipped through v0.4.0.
The version defines the inventory, not a release-preparation deadline.
The [service inventory](iac-services.md) distinguishes baseline support from implementation work.
Existing [Stack](../guides/stacks.md) and [Terraform REST examples](../../examples/terraform/basic/README.md) keep their documented limits until the new behavior is implemented.

## Two frontends, shared resource rules

Native Stack uses versioned YAML and the existing Git synchronization controller.
Terraform uses a dedicated provider implemented in this repository under `terraform-provider-firecrab/`.
It uses the Go Terraform Plugin Framework, protocol 6, initially pinned to v1.19.0.
The initial client requirement is Terraform >= 1.11 and < 2.0, including write-only arguments.
Test the minimum supported Terraform version and the development version (currently 1.15.5) before claiming compatibility.
The provider module and checksums must pin build dependencies before stage 4 begins.
Provider distribution begins with a reproducible local installation; public registry publication is a separate action.

The provider exposes typed `firecrab_<inventory-id>` resources and read-only data sources for existing dependencies.
It calls public Firecrab APIs directly, without shell commands or database access.
It does not invoke `terraform` on the API host or translate HCL into an unreviewed Git source.
Both frontends reuse server-side validation, ownership, planning, execution and recovery rules.
Neither frontend may bypass an existing parent controller's resource locks.
Host, controller and native-management resources keep their inventory scopes.
A native management broker must expose outer-host settings through an authenticated control boundary.
The guest API cannot modify its own outer VM by pretending those settings are guest resources.

All persisted, user-writable fields in the v0.4.0 API/configuration contract are in scope.
The matrix must identify fields needing new API support instead of silently omitting them.
Computed metrics, task results and application contents are not desired configuration.
The inventory explains those operation/data boundaries for every service family.
Unsupported fields and versions fail validation before mutation.

## Resource references and ordering

Every declaration has a stable kind and logical key independent of map order and file path.
A managed reference identifies another logical resource and creates a dependency edge.
An external reference carries its exact resource identity and host/controller scope.
Names never silently adopt an existing resource.
References to an already-installed template also include its immutable version and boot hashes.
OCI images use SHA256 digests; registry credentials remain separate references.

Resolve and validate all references, cycles, scope and required capabilities before admission.
Create dependencies first; stop and delete dependents first.
Bindings such as volume attachments carry their own ownership or belong to the exact parent specification.
A parent controller retains ownership of generated children, including replicas and pool/environment VMs.
Cluster resources keep exact host-qualified identities and existing placement limitations.
No new cross-host networking, database provisioning or external cloud platform is implied by IaC support.

## Desired state and readiness

New standalone VM declarations default to stopped for compatibility.
An explicit running state starts the VM and waits for confirmed guest/network readiness.
Stop waits for runtime shutdown; paused or uncertain execution requires an explicit supported transition.
A running service waits for the accepted operation's healthy replicas and required routes.
A stopped service keeps its saved configuration and stops only its owned replicas.
A ready Queue engine or database binding requires its existing service-specific readiness checks.
An accepted HTTP request is not deployment success.

Changing CPU/RAM or attachments follows the existing inactive-VM rules.
When interruption is required, plans report it and apply requires explicit permission.
Mutable changes preserve identities and application disks.
Immutable changes are replacement actions and require explicit replacement consent.
A matching declaration produces no new deploy, restart, token, task or data operation.
Automatic tag watchers and schedulers require explicit delegation of the fields/state they own.
Git, Terraform and tag watchers cannot all act as the image writer for the same service.
A running/stopped state managed by a schedule is either delegated explicitly or reported as a conflict.

## Ownership, import and management transfer

The server records an exact resource identity, managing controller and generation.
Terraform records stable resource IDs and durable operation IDs in state/private state.
State-backend locking protects one Terraform state; server ownership protects competing controllers.
A lock on one state is not authorization to overwrite a resource owned elsewhere.

Import binds an exact UUID, or the existing stable alias/version/provider key for services without UUIDs.
Host/project namespaces are part of that identity; import never recreates the resource.
It refreshes public configuration and never reveals credential values to populate state.
Import rejects active operations, mismatched scopes and competing owners.
Existing resources can be referenced read-only without adoption.
Management transfer explicitly releases the old writer and adopts the same exact identity.
The first matching plan after transfer must be a no-op.
Pausing Git polling alone does not release Git ownership.
Renaming a Terraform address uses a state move; it must not rename the resource implicitly.

## Admission, retries and recovery

Validate all resources before mutation, including capacity, permissions and external references.
Bind a reviewed plan to desired configuration, immutable inputs, ownership and observed generations.
Reject a stale plan before applying changed assumptions.
Reserve exact create identities and an operation ID durably before dispatch.
The same operation ID and request fingerprint recover the original result.
Conflicting contents with the same operation ID return a conflict.

Clients wait on the recorded operation with configurable bounded timeouts.
Disconnection or client timeout does not silently cancel accepted server work.
After an unknown response or restart, query the original operation before retrying.
Never allocate a fresh resource solely because a response was lost.
Partial success remains visible with per-resource identities and terminal errors.
Recovery does not erase reservations or replay completed data operations.
External deletion is observed distinctly from temporary transport failure.
A failed lookup or invalid credential cannot become an empty desired configuration.

## Destruction and retained data

Plans distinguish unlinking configuration, stopping runtime, replacing resources and deleting data.
A removed Stack declaration retains its resource unless pruning is explicitly enabled.
Terraform destroy expresses resource removal, subject to the same ownership/dependency checks.
It stops owned runtime dependencies without requiring separate manual API calls.
Data deletion requires an additional resource-specific consent, not just stop/replace permission.
A retained volume, root disk or Workspace keeps a discoverable exact identity for later import or cleanup.
Detaching a volume does not erase it.
Disconnecting a bucket/database/provider does not delete its external data or account.
Secrets, tokens, queue messages and retained workspaces have explicit per-resource policies.
Deleting a parent cannot delete borrowed dependencies or another owner's resources.

## Credentials and secret inputs

Connection metadata is declarative; credentials are injected at apply time through a separate channel.
YAML/Git uses host-resolved credential references, not literal values or arbitrary file includes.
Terraform uses sensitive write-only fields paired with a non-secret rotation version.
Write-only values may come from ephemeral variables and never enter plan or state.
Refresh preserves redacted metadata and version markers without re-reading secret plaintext.
Changes to the version marker request rotation; ordinary refresh never rotates credentials.
A private credential channel uses existing authenticated transport and encrypted/owner-only storage rules.
Logs and diagnostics use static categories rather than request values or provider response bodies.

Auth/provider credentials, service Secrets, database URIs, registry logins, tunnel tokens and TLS private keys follow this rule.
One-time generated CI tokens need an explicit secure delivery destination; they cannot be recovered by Terraform refresh.
The provider must not return a token as an ordinary computed state attribute.
Interactive account consent and initial CA/endpoint bootstrap remain external prerequisites.
IaC configures the service connection and verified reference after those prerequisites exist.

## Acceptance for each inventory row

Record supported fields and immutable/update behavior against the pinned v0.4.0 API baseline.
Test Stack and Terraform create, observe, update, import, reference, transfer and destroy as applicable.
Verify dependency ordering, health, repeated no-op, drift, failed credentials and safe cleanup.
Inject request loss, interruption, API restart and concurrent writers.
Inspect plans/state/logs for prohibited credential values.
Retain exact build/runtime versions and live evidence for Linux/KVM and controller/worker operations.
A fixture test or three-resource demo cannot establish full service coverage.
