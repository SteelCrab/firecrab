# Original Stack deployment credentials

This is a stage 2 prerequisite in [#548](../reference/iac.md).
It connects existing MicroService Secret references to the existing Stack
planning and deployment paths. It does not implement all-service Stack coverage
or the Terraform provider.

## Plan and admission

A service declaration uses `config.secrets` to map environment names to exact
existing PipeSecrets UUIDs. Names and literal credentials are not accepted as
credential references.

The plan records `inputs.serviceSecrets` with each UUID, vault version, endpoint
and ownership generation. `inputs.serviceSecretReleases` records proven original
references of the observed release. Neither contains values or value hashes.
A missing reference fails planning. An unreadable vault is an error, not an empty
set of credentials. Stacks without credential references do not open the vault.

Before admission, the API validates the complete merged guest environment and
persists its original values in the existing encrypted vault. The private scope
binds the parent apply, Stack registration, service, deployment child, exact
configuration and approved references. Private snapshots alone grant no authority.

The final inventory check and parent-journal commit hold the vault lock.
Rotation or management transfer before admission invalidates the earlier plan.
All vault/SQLite boundaries acquire the vault first to avoid lock inversion.
A failed metadata commit can leave private prepared data. It cannot authorize
deployment or replace the original input of the same operation UUID.

## Execution and cleanup

Only the accepted parent running the exact service step can use a prepared
snapshot. Initial and rolling deployments use the same original child identity.
Their execution does not resolve current Secret values again.
Rotation or deletion after admission therefore cannot substitute newer values.
Replica boot uses the original encrypted deployment environment after restart.
The public VM record and deployment journal retain references, never values.

A changed Secret version appears as `secretVersions` in a later plan and requests
deployment with the new approved inputs. Matching proven versions produce no
credential deployment. A borrowed reference's ownership transfer changes the
plan proof, but does not alone request a new deployment of identical values.
Unproven legacy release versions require an explicitly reviewed deployment.

Cleanup removes captured values and retains a private identity tombstone.
An old worker cannot recapture the current value under that cleared identity.
Legacy unfinished Stack work without a proven snapshot fails instead of resolving
new credentials. Explicit recovery of interrupted parent workflows remains part
of the remaining lifecycle work.

Completed parent request retries return their recorded status before reading
current inventory or credentials. Deleted references or a subsequently unavailable
vault cannot turn an original acknowledgement into a fresh deployment.

## Verification scope

Tests use real SQLite files, the encrypted vault and local API requests.
They cover stale planning, missing references, private parent authority, changed
configuration, original input retention, restart and cleanup tombstones.
Local HTTP health fixtures verify service acknowledgement recovery.
These tests do not claim a real guest boot, live registry access, live provider
credentials or both-workflow coverage for every inventory row.
The #548 integration hold remains in effect.

The [Linux Release evidence](../assets/iac-548-stack-secret-linux-evidence.json)
records the exact original-Secret source and executable/log hashes.
