# Offline project backup transfer

This is an implementation step for #550. It copies and checks a completed matched
backup. It does not import the project, register destination resources, or start a VM.
Different-host restoration and binding validation remain open.

Run the tool as the owner of the private controller and backup files. Supply a new
destination directory inside an existing owner-only directory:

```sh
python3 scripts/project-backup-transfer.py export \
  --control /private/firecrab/control.db \
  --workspace-parent /private/firecrab/workspaces \
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
expose it as a downloadable workspace artifact.

On the receiving host, run:

```sh
python3 scripts/project-backup-transfer.py inspect /private/transfer/checkpoint
```

Inspection checks the matched hashes, ext4 UUID and length, SQLite integrity and
project/control snapshot identities. It does not prove destination storage, VM,
Sandbox migration authority or generation compatibility. Do not install the files
manually as a completed recovery. Destination registration and restartable import
are still required. No tests or CI were run for this increment at the user's request.

## Inspect the destination

Before attempting a future import, inspect the destination controller and intended VM:

```sh
python3 scripts/project-backup-transfer.py plan-import /private/transfer/checkpoint \
  --destination-control /private/destination/control.db \
  --vm DESTINATION_VM_UUID --volume-name project-volume
```

This read-only inspection reports project/database/volume/VM identity collisions,
source task reservations, private view/draft counts and retained session counts.
It does not copy credentials or display private record contents. A stopped registry
state alone does not prove that the VM process is stopped. A conflict-free report is
not an admission, a resource reservation or proof that restoration is safe. The
actual journaled destination registration and matched DB/volume installation are
still being implemented. Tests and CI are deferred at the user's request.
