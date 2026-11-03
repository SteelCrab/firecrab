# Offline project backup transfer

This #550 implementation provides export, inspection and explicit destination
registration with matched DB/volume installation. It is an operator CLI workflow.
It does not start a VM. Genuine different-host execution and interruption acceptance
are still pending. Tests and CI are deferred at the user's request.

## Export a matched checkpoint

Run the tool as the owner of the private controller and backup files. Supply a new
destination directory inside an existing owner-only directory:

```sh
python3 scripts/project-backup-transfer.py export \
  --control /private/firecrab/control.db \
  --workspace-parent /private/firecrab/control.sandbox \
  --project PROJECT_UUID --checkpoint CHECKPOINT_UUID \
  --destination /private/transfer/checkpoint
```

The workspace parent must contain `project-backups/`. The destination must not
exist. Export holds the project's backup operation lock, checks the live retained
receipt and binding, verifies the archive, copies the matched files, verifies the
copy and checks the binding again. Files are owner-only. Sparse zero ranges remain
sparse. An interrupted export leaves an `INCOMPLETE` marker and its files for
inspection; it never overwrites or removes an existing destination.

Transfer the directory using an operator-controlled secure mechanism. It includes
`control.db`, which can contain credentials and data for other projects. Protect
it as a full controller backup. Do not place it in the shared project folder or
expose it as a downloadable workspace artifact. Keep this directory unchanged
until destination registration and its subsequent workload acceptance are complete.

On the receiving host, run:

```sh
python3 scripts/project-backup-transfer.py inspect /private/transfer/checkpoint
```

Inspection checks the matched hashes, ext4 UUID and length, SQLite integrity and
project/control snapshot identities. This command alone does not reserve resources,
prove the VM is inactive or authorize installation. Do not install individual files
manually as a completed recovery.

## Inspect the destination

A read-only preliminary report is available:

```sh
python3 scripts/project-backup-transfer.py plan-import /private/transfer/checkpoint \
  --destination-control /private/destination/control.db \
  --vm DESTINATION_VM_UUID --volume-name project-volume
```

This reports identity collisions, source task reservations, private view/draft
counts and retained session counts. The importer can resolve ordinal collisions
reported by this preliminary inspection. It does not display private record contents.
A conflict-free report is not an import admission. The `import` command performs
additional checks and a durable registration transaction.

## Prepare the destination

1. Use Linux, Python 3, `e2fsck` and a libc with `renameat2`. Destination filesystems
   must support SQLite durability, advisory locks and non-replacing atomic rename.
   Use local persistent disks. Run as the destination Firecrab service user.
2. Initialize the destination controller once with the updated #562 code. This
   creates the import journal and startup guard. An older controller is unsupported.
3. Register a destination MicroStorage pool. Its path must be absolute and owned by
   the service user. It must not be writable by other users. Prepare a new **Created**
   user MicroVM that has never started. It must have no project/service/cluster owner,
   no overlapping `/workspace/shared` mount and an available data-disk slot.
4. Reconcile source tasks and file mutations. Only settled task history is accepted.
   Finish any prepared/cutover Sandbox migration before taking the transferred backup.
5. Stop/quiesce the source project before cutover. Keep it stopped while the destination
   becomes authoritative. The acknowledgement below records an operator decision;
   this tool does not control or prove the state of another host.
6. Stop the entire destination controller. The importer acquires the controller's
   whole-process database lock and holds it throughout installation and registration.
   Do not run another Firecrab version, database writer or VM manager against these
   resources during import. Existing destination workloads are outside this procedure.

The destination project, DB, volume UUID and resource directories must be unused.
The requested volume name must also be unused. Task UUIDs and attempt UUIDs must not collide with existing destination history.
For controller-backed and project-DB-backed history, the importer assigns ordered
catalog ordinals above existing destination entries. It journals the mapping before
copying files and reuses it on retry. For project-DB-backed history the private task
index is updated to match. Restoring the original imported checkpoint later translates
its staged task index using the retained mapping. The original archive is not changed.
The restore receipt records the translated DB hash for interrupted recovery. If newer
project-DB tasks exist, restoring that original checkpoint is rejected; create and
restore a newer checkpoint instead. This avoids losing their catalog-backed history.
Task identity remapping, in-place replacement and merging two histories are outside this
workflow.

Allow space for both the live pair and a retained source archive. Capacity checks
sum logical file lengths per filesystem, including two volume copies when they
share one filesystem. Private control/project/backup directories must be owner-only.
The registered pool and existing volume namespace may be readable by other users,
but must be owner-controlled and not writable by others. Newly created directories
use mode `0700`; files use `0600`.

## Register and install

Generate one nonzero canonical UUID for this import. Save it and reuse it for retries.
It must differ from the source checkpoint UUID and all existing backup/cleanup IDs.

```sh
python3 scripts/project-backup-transfer.py import /private/transfer/checkpoint \
  --destination-control /private/destination/control.db \
  --operation IMPORT_OPERATION_UUID \
  --vm DESTINATION_VM_UUID \
  --storage DESTINATION_MICROSTORAGE_UUID \
  --volume-name project-volume \
  --confirm-source-quiesced
```

The importer records the immutable request before installing files. It creates and
syncs private staged copies, checks each original checksum, sanitizes the live DB,
checks the live ext4 copy with `e2fsck -f -p`, and records a complete file plan. The
filesystem journal and safe unattended fixes can change only this staged live copy;
the protected source archive stays byte-for-byte unchanged. Only clean/corrected
exit codes 0 and 1 are accepted. Other results require operator repair. Installation
uses non-replacing atomic rename. A changed or existing conflicting file stops the
operation. Existing projects and disks are never overwritten.

Paths derive from the destination registration, not from source-host absolute paths:

- Private DB: `control.sandbox/projects/PROJECT_UUID/project.db` next to `control.db`.
- Live disk: `REGISTERED_POOL/volumes/VOLUME_UUID/data.ext4`.
- Private source archive: `control.sandbox/project-backups/PROJECT_UUID/CHECKPOINT_UUID/`.

After all files are installed and synced, one SQLite transaction registers the
project, ready volume attached at `/workspace/shared`, workspace binding, task
catalog/route, historical-task restrictions and backup/import receipts. The project,
database and volume UUIDs are preserved. The VM and pool are the selected destination
resources. The generation advances from the checkpoint generation by one. Runtime
verification starts empty. A retained restore receipt requires stale browser writes
to acknowledge the new generation.

The original `control.db` remains only inside the private source archive. It never
replaces the destination controller. Models, credentials, host access policies and
unrelated project rows are not imported into the live controller. Project ownership
is preserved. Review destination grants explicitly. Source host paths, default model
selection and managed Environment binding are cleared from the live project settings.

Private views, drafts, version history, saved instructions, skill records and session
history remain in the project DB. Editor tokens are rotated. Source runtime proofs
and terminal input leases are revoked. Editors and terminals become interrupted
history; no process or command is resumed. Start a new destination session explicitly.
Installed skills and saved selections require destination runtime/provider qualification.

Canonical project-DB tasks retain their verified migration marker and destination
route. Legacy controller-backed tasks are copied only for the selected project.
Historical task/attempt identities and immutable admitted context remain. Source-host
workspace pointers are cleared in the live records. Imported tasks cannot execute,
apply changes, reconcile, cancel, retry or become dependencies for new execution.
Read their retained results and submit new tasks on the destination. Old external
attempt work directories and separate migration-source archives are not copied.
The original control snapshot remains available as private recovery evidence.

## Resume an interrupted import

The controller refuses startup while an import is `staging` or `prepared`. This
prevents serving a partly installed DB/volume pair. Keep it stopped. List incomplete
operations without the transfer package or a running controller:

```sh
python3 scripts/project-backup-transfer.py import-status \
  --destination-control /private/destination/control.db
```

Use `--operation IMPORT_OPERATION_UUID` to read one incomplete or completed receipt.
The response exposes operation/resource IDs and phase, not private file contents or
credentials. It reports journal state only; it does not verify the staged files or
stop a running controller. Restore missing storage access, permissions or free space,
then rerun the identical import command with the same operation UUID, VM, pool and
volume name. The `staging` phase requires the original checkpoint package.

- `staging`: prepare new private copies. Earlier partial files remain for inspection.
- `prepared`: verify every installed/staged file against the retained plan. Publish
  only missing members, then retry the single registration transaction. If the
  original transfer directory is absent, the importer reads its journaled archive
  copies from the private staging and checkpoint directories. It checks all six file
  proofs, the matched checkpoint, SQLite integrity and resource identities before
  continuing. Missing, conflicting or changed files stop recovery. The destination
  controller must remain stopped and the same source cutover acknowledgement and
  command arguments are required.
- `complete`: return the retained receipt. Do not rewrite the live DB, disk or later
  changes. If the original transfer directory no longer exists, an identical retry
  can return the receipt without reading it. If the directory still exists, its
  manifest must match. `import-status` can also read the completed receipt without
  the package or the original command arguments.

Changing the registered pool path, filesystem identity, immutable request or prepared
bytes fails closed. A different import cannot bypass an incomplete operation. Do not
manually delete the journal row or change its phase to unblock controller startup.
There is no automatic rollback or deletion of partial installation evidence.
Retain all files before attempting recovery or explicit abandonment.

### Abandon an unregistered import

If a `staging` or `prepared` import cannot resume, the operator can explicitly abandon
it while the updated controller is stopped. It does not require the original transfer
package. It cannot abandon an import after project or volume registration is committed.

```sh
python3 scripts/project-backup-transfer.py abandon-import \
  --destination-control /private/destination/control.db \
  --operation IMPORT_OPERATION_UUID --confirm-abandon
```

The command checks the controller lock, registered pool identity, resource bindings,
private ownership and expected directory contents. For `prepared` imports it verifies
the entire six-file plan and its hashes, including files already installed in their
final locations. It journals directory identities before moving anything. Staging and
resource directories are renamed to private `.abandoned-import-IMPORT_OPERATION_UUID-*`
entries beside their original namespaces. Nothing is deleted. Unknown, duplicated,
missing or changed files and registered project/volume resources stop the operation
for operator inspection.

If interrupted, rerun the same command with the same operation UUID. The controller
remains blocked until every journaled move and the final receipt are complete. A
completed abandonment has phase `complete` and outcome `abandoned` in `import-status`.
The old import UUID cannot be resumed. Start a new import with a new UUID if needed.
The quarantined evidence remains private and consumes storage until an operator
explicitly cleans it. This feature has not yet been tested on a live host.

### Clean an abandoned import quarantine

Only after an import is recorded as `abandoned`, an operator can inspect and explicitly
remove its quarantine while the destination controller is stopped:

```sh
python3 scripts/project-backup-transfer.py purge-abandoned \
  --destination-control /private/destination/control.db \
  --operation IMPORT_OPERATION_UUID
```

Without confirmation this prints each quarantine role and relative filename, plus the
number of files and logical bytes. A pending retry lists the original plan, including
files already removed. The preview does not write a cleanup plan or delete data. It scans and hashes the private quarantine, so a
large volume can make this step slow. The result is a preview, not a reservation.
Review it, then repeat the command with `--confirm-delete` to permanently remove the
listed quarantine files. The command never targets the source transfer, registered
project DB, live volume, other projects, VM or retained backup catalog.

Before the first deletion the controller records the exact file hashes and identities.
It checks for unknown entries and changed identities, removes only recorded files,
syncs each parent directory and removes only recorded empty directories. Interrupted
cleanup can be resumed with the same operation and `--confirm-delete`. A completed
receipt returns the saved result without touching newer files. `import-status` reports
whether quarantine cleanup is pending or complete. Missing storage, changed files,
symlinks, hard links or unknown entries stop cleanup for operator inspection.

This is an irreversible cleanup of the abandoned quarantine. Keep any evidence that
is still needed before confirming. The implementation has not been tested or exercised
on a live host. Permanent deletion of an active retained project is a separate
requirement and is not implemented by this command.

Private `.project-import-IMPORT_OPERATION_UUID` staging directories remain beside
project, backup and volume namespaces. Partial copies are outside live resource and
checkpoint directories. They are not deleted automatically. More than 128 retained
staging entries stops another copy attempt and requires operator inspection. Do not
remove a file named by a prepared import plan.

## Resume service and qualify the workload

After import reports `complete` or abandonment reports `abandoned`, start the updated
controller using its normal service procedure. Only a completed import registers the
project and volume. For a completed import, refresh project state and generation.
Start the selected VM explicitly, then run workspace reconciliation to verify the mounted ext4 UUID and
project/database/import marker. Configure destination model connections and grants
before creating new agent tasks. Inspect restored files, version history, private
drafts and session history under their original caller identities.

These steps describe required operator validation. They have **not** been executed
for this increment. Real second-host restore, interrupted-copy/rename/commit recovery,
service restart, production service-user and multi-user acceptance remain open in
#550. PR #562 remains Draft and unmerged.
