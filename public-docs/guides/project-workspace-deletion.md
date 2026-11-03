# Permanent retained-project deletion

This is an offline operator workflow for #550. It deletes one registered project's
managed private database, task workspaces, project checkpoint/backup archives and
files on its retained volume. It preserves the borrowed VM registration, borrowed
volume registration, volume UUID and volume size. The volume is detached and its
contents are replaced by a newly formatted empty ext4 filesystem.

**This implementation has not been tested or run on a live host.** Do not treat it
as accepted production recovery. PR #562 stays Draft and unmerged until all #550
implementation and validation are complete.

## Scope

The command removes project-owned files under the controller's project workspace,
project backup and Sandbox attempt directories. This includes private views, drafts,
session history, task records, instructions, skills, local checkpoints and protected
project backup archives. It removes the project's control registration, task catalog,
backup policies, the project's default AI model selection and project grants. Shared
AI provider connections remain. It retains a deletion tombstone and prevents that
project UUID from being reused.

The borrowed VM is not stopped, deleted or reformatted by this command. Its other
disks and guest files are not modified. The borrowed volume registration and UUID are
preserved, but **all files on that volume are deleted**, including anything placed there
outside Firecrab. Host source directories, external Git repositories, provider accounts,
other projects and independent backups/snapshots are not modified. Copies of project
data in other projects' controller snapshots or external backups remain outside this
operation. Delete those separately under their own retention policy.

This is logical removal of Firecrab-managed data. Unlinking and formatting do not
guarantee physical media sanitization on SSDs, snapshots, copy-on-write filesystems,
replicas or existing external backups. SQLite secure-delete and a WAL checkpoint reduce
retained controller pages but are not a physical secure-erasure guarantee.

## Preconditions and preview

1. Use Linux, Python 3, `mkfs.ext4`, Linux `renameat2` and mounted `/proc`.
   The local volume filesystem must support `O_TMPFILE`.
   Run as the Firecrab service user from the same working directory and with the same
   `FIRECRAB_STORAGE_ROOTS` configuration as the controller. Initialize the updated
   controller once, then stop it. Do not use an older binary during this operation.
2. Stop the borrowed user VM and confirm its native process is gone. The CLI checks
   the recorded `created` or `stopped` state. The explicit acknowledgement below is
   an operator assertion; the CLI cannot prove another supervisor stopped the VM.
3. Finish or reconcile active Sandbox tasks, pending file operations, migrations,
   backup and retention cleanup. Delete owned Environments and retire live preview
   bindings first. Clean any abandoned import quarantines for this project first.
4. Resolve the absolute configured pool path holding the selected volume. The CLI
   matches it to the controller's configured root ID or registered MicroStorage path.
   The service user must own the storage and project directories. Symlinks, hard links,
   nested mounts, other filesystem objects and changed identities stop deletion.
5. Inspect and retain any backup or file that must survive. The operation deletes
   protected project backups as well as unprotected project backups. The borrowed
   volume itself remains, but its complete filesystem contents will be replaced.

Use a fresh canonical nonzero UUID as the deletion operation ID. Save it for retries.
Obtain the current workspace generation and project/volume/VM identities through the
normal authenticated workspace API before stopping the controller. Preview using the
same arguments intended for execution:

```sh
python3 scripts/project-backup-transfer.py delete-project \
  --control /private/firecrab/control.db \
  --project PROJECT_UUID --operation DELETION_UUID \
  --generation CURRENT_GENERATION --pool /private/storage \
  --confirm-vm-stopped
```

Preview reports the project, DB, VM and volume IDs, generation, project file count,
logical byte count, backup receipt counts, a `previewDigest` and up to 100 relative file names. It does
not reserve or delete anything. If `filesTruncated` is true, the list is incomplete.
Inventory and hashes can take a long time for large volumes and backups. The preview
captures file and resource proofs. Execution checks them again and rejects a changed
plan. Another preview is required if they differ.

## Execute and resume

Only after reviewing the preview and preserving required data, repeat the exact command
with `--confirm-delete --confirm-plan PREVIEW_DIGEST`. This confirms deletion of all
listed project areas, project backups and the **entire borrowed-volume filesystem
contents**. Use the exact digest returned by preview. The command takes the
controller's exclusive scheduler lock and existing project/service/backup locks. It
records the immutable operation, file identities and hashes before changing data.

It records a new unnamed image inode before publishing its temporary filename. An
interruption before publication leaves no untracked file. It creates a blank ext4
image with the same volume UUID and size, records the image proof, installs it
without overwriting an unexpected file and keeps the old
image until project-owned files are removed. It deletes only recorded files, rejects
unexpected entries and removes only recorded empty directories. It then removes the
old volume image, detaches the volume in the control DB, removes project/task/backup
metadata and project grants, checkpoints the control WAL and records completion.
VM and volume resource records remain. The old project UUID stays reserved.

While deletion is incomplete, the updated controller refuses startup. Restore the
required storage access or resolve the reported conflict, then rerun the **same**
command with the same operation UUID, project UUID, generation, pool and confirmation
flags, including the saved preview digest. The deletion status response includes this
digest if the original response was lost. Do not manually remove or edit the deletion journal. Once erasure starts there
is no automatic rollback. A completed retry returns the retained receipt and never
repeats deletion against later files.

Query saved progress without starting the controller:

```sh
python3 scripts/project-backup-transfer.py deletion-status \
  --control /private/firecrab/control.db --operation DELETION_UUID
```

Omit `--operation` to list incomplete deletion operations. Phases are `staging`,
`prepared`, `erasing`, `finishing` and `complete`. An unexpected file, directory,
mount, symlink, hard link, changed checksum, storage identity or resource binding
stops the operation. Preserve the journal and inspect the conflicting resource.

Only after a `complete` receipt may the updated controller be restarted. The retained
volume is then available and empty. A new project requires a different project UUID.

## Validation boundary

No deletion, test, build or CI was run for this implementation. Real service-user,
VM stop/start, interrupted file removal, volume replacement, project isolation and
full #550 acceptance are still required at final integration. External backups and
forensic media sanitization require independent procedures.
