# Database migration ownership and recovery

A migration owns one immutable version and SQL checksum. Its exact management
identity is `database_migration/database_uuid:job_uuid`. The original job UUID
continues to identify the same version after transfer or connection removal.
Import never runs SQL. SQL stays in private persistence. Public observations
return the version and SHA-256 `versionChecksum` without the SQL text.

The native migration endpoint accepts the existing `requestId`, `version`, SQL
and maintenance acknowledgement. It records one definition for each version of
a source target. A second saved alias cannot bypass that reservation with a new
UUID or different checksum. Retry the original operation instead.

The source session takes a target lock before schema initialization and migration
SQL. PostgreSQL DDL uses a transaction. MySQL retains an `applying` ledger entry
when nontransactional DDL has an uncertain outcome. A stale worker must pass its
original running execution-generation check after acquiring the source lock.

Observe one job with `GET /api/pipe/database-jobs/JOB_UUID`. Observation does not
load newer connection settings or replay SQL. A completed POST retry returns its
original receipt even after the parent connection is removed or ownership changes.
Changing its version or SQL with the same UUID returns a conflict.

Only a known failure before maintenance effects can be retried through
`POST /api/pipe/database-jobs/JOB_UUID/retry` with the observed
`expectedGeneration`. The same job UUID, version and SQL are retained. A stale
attempt is rejected. Terraform ownership also rejects this public writer.
Unknown DDL outcomes cannot use that route. Inspect the original operation and
its recorded recovery path. Refresh never retries uncertain SQL automatically.

Active or uncertain jobs prevent transfer of the migration and source target.
Transfer of a finished migration preserves its execution history. Releasing the
migration's ownership retains schema and history. There is no reverse SQL or
implicit database deletion.

This shared prerequisite does not complete declarative execution through
expanded Stack or the dedicated Terraform provider. Target-wide coordination
with every restore/configuration writer and complete workflow acceptance remain
tracked by issue #548.
