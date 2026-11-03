# Destination project import — 2026-11-04

Delivery: [#550](https://github.com/SteelCrab/firecrab/issues/550),
[Draft PR #562](https://github.com/SteelCrab/firecrab/pull/562),
`feat/550-ai-canvas` → `release/v0.5.0`. No merge is authorized before full issue
implementation and acceptance are complete.

## Implemented behavior

The operator CLI now includes `import`, not only export and destination inspection.
[Guide and recovery procedure](../guides/project-backup-transfer.md).

- Require a stopped destination controller, its whole-process database lock, an
  initialized updated schema, a never-started Created user VM and registered storage.
  Source cutover quiescence requires explicit operator acknowledgement.
- Reject existing project/database/volume identities, paths, task identity collisions,
  VM ownership, overlapping mounts and exhausted disk slots. Destination files are
  owner-controlled; no existing resource is replaced.
- Validate original matched hashes, SQLite identities/integrity, ext4 UUID/length,
  settled tasks, task admissions and canonical migration authority. Stage live and
  archive copies, revoke source execution leases, then check the copied filesystem.
- Journal admission, staged file hashes and installation phase. Sync files and
  directories before non-replacing atomic rename. An incomplete import blocks API
  startup until the same operation completes.
- Commit project registration, volume attachment, workspace generation, task routes,
  task catalog, read-only imported history and retained backup/restore receipts in one
  durable controller transaction. Preserve project/database/volume IDs and advance
  the generation once. The new runtime must establish fresh mount proof.
- Preserve private layouts, drafts, file history, instructions, skills and session
  history. Rotate editor tokens, revoke terminal control leases and interrupt retained
  sessions. No editor, PTY, task or shell command is automatically relaunched.
- Retain the unmodified matched source archive privately. Copy only project-specific
  controller task rows when legacy routing remains authoritative. Clear source-host
  paths and provider/Environment bindings from the live project settings. Do not
  import unrelated control records, credentials, model proofs or host grants.
- Imported attempts remain read-only history. Their source-host workspace pointers
  are removed from live records. Execution/apply/reconciliation/cancellation/retry
  and inheritance as new task dependencies are denied by the API.
- Same-ID retry verifies prepared artifacts and installs only missing files. A completed
  retry returns its receipt without overwriting later destination writes. Partial
  staging evidence is retained outside live resource/checkpoint directories.

## Scope and limits

This is an offline CLI path for a fresh destination project and volume. There is no
in-place merge, task identity remapping or automatic source-host shutdown. The existing
read-only `plan-import` command is preliminary; `import` has stricter admission checks.
Task and attempt UUIDs must not collide with the destination catalog. The original source
archive stays available as private evidence. External task work directories and old
migration-source archives are not copied. Models, skills, grants and runtime support
need destination qualification before new tasks can run.

The source project owner and private caller identities remain. Destination users do
not inherit access simply because their data is present. Terminal/editor histories
are preserved without old process ownership. Source absolute paths are never used
as destination live resource locations.

## Validation status

**Tests, builds, lint checks and CI were not run for this increment at the user's
request.** Rust formatting and source/schema inspection were performed. No importer
was executed. No production controller, VM, project DB or volume was changed. No
live second-host recovery, interrupted install, restart or post-import workspace
operation is claimed as verified.

The new journal and startup guard are additive. The destination must be initialized
with the updated controller once before stopping it for import. Older binaries must
not be used to bypass an incomplete import. There is no automatic rollback or cleanup
of interrupted evidence. The guide describes exact-ID resumption and operator repair.

#550 remains incomplete. Keep genuine different-host binding/lifecycle acceptance,
real ChatGPT/Claude execution, native editor recovery and production service-user/
multi-user qualification unchecked. PR #562 stays Draft with no auto-merge.

## Task catalog ordinal collisions

Both controller-backed and project-DB-backed histories now receive destination
ordinals after existing catalog entries. Task/attempt IDs and relative order stay
unchanged. The mapping is committed in the import journal before files are prepared.
Retry uses the same mapping. The private project DB index and destination catalog
use the same ordinals. Older incomplete journals keep their original ordinal plan.

The original protected checkpoint remains byte-for-byte unchanged. A later restore
copies and verifies that checkpoint, then translates its staged private index with
the recorded mapping. Its restore receipt records the resulting DB hash before
installation. Interrupted installation checks that hash on retry. Restore verifies the
mapping against the destination catalog and rejects the original checkpoint if newer
project-DB tasks would be lost. Other checkpoints continue using their original hashes.
UUID collisions remain rejected.

Tests and CI were skipped at the user's request. Real import, restore, interruption,
restart and multi-user acceptance remain open.

## Import recovery status

`import-status` reads incomplete import identities and their journaled phase without
a running controller or the transferred package. An explicit operation UUID can read
a completed receipt. It does not mutate the journal or prove staged-file integrity.
A completed identical import retry can return its stored receipt if the transfer
directory is gone; existing directories must still match the recorded manifest hash.
Destination preflight now reports a previously started VM as ineligible, matching
import admission. Tests, CI and live-host execution remain deferred.

## Prepared import recovery without transfer directory

When a `prepared` import loses its original transfer directory, the same import
command can resume from the journaled archive copies. It first checks destination
pool identity, private paths, the entire six-file install plan and each file hash.
It then validates the matched archive, SQLite identities and task authority before
publishing any remaining files. Archive copies may be split between private staging
and their final checkpoint directory after an interrupted rename. Unknown SQLite
sidecars are rejected. Missing or altered evidence fails closed. Earlier `staging`
operations still require the original package. A present but invalid source package
is not bypassed.

Tests and CI were skipped at the user's request. Live destination-host execution,
interrupted installation and full #550 acceptance remain pending.

## Abandon a pre-installation import

An operator can explicitly abandon a `staging` import without its source package.
The stopped-controller CLI validates the registered pool and resource identities,
then journals directory identities before moving the import's empty resource
directories and private staging directories into same-filesystem quarantine names.
It retains all partial files. A crash leaves the startup guard active; the same
operation resumes the recorded moves. Only after those moves does it record the
`abandoned` outcome and release the startup guard. Imported-checkpoint lookup
excludes abandoned receipts. The initial implementation did not support prepared
imports; the following extension adds that support before registration.

This feature has not been tested or executed. Tests and CI remain deferred until
final integration; live interruption and service-user validation remain open.

## Abandon a prepared import

An explicit offline abandonment can also quarantine an unregistered `prepared` import.
The controller and pool identity, unregistered project/volume, all six journaled
file paths and hashes, checkpoint hashes and private directory identities are checked
before the move plan is committed. Staged and already installed files remain in their
owner-only directories. Each directory is renamed to its operation-specific quarantine
location without replacement or deletion. A retry verifies all files even if directories
are split between original and quarantine locations. The controller startup guard stays
in place until the final abandoned receipt is durable. Unknown, changed, duplicated or
missing files stop the operation. A committed project registration cannot be abandoned.

Tests and CI were skipped at the user's request. Real prepared-import interruption,
abandonment, service restart and full #550 acceptance remain open.

## Abandoned quarantine cleanup

The offline `purge-abandoned` command previews an abandoned import's private
quarantine file count and logical bytes. With explicit deletion confirmation it
records bounded exact file identities, hashes and directory identities before
unlinking. It rejects unknown entries, symlinks and hard links. Interrupted cleanup
resumes the same plan. A completed receipt retains its file/directory proofs and does
not touch subsequently created files.
The controller must be stopped, and the registered pool identity must still match.
Live projects, volumes, VMs, transfer packages and backup catalogs are not cleanup
targets. The command does not implement permanent deletion of a retained project.

No cleanup was performed and no tests or CI were run for this implementation. Live
interruption and service-user acceptance remain open in #550.
