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
