# Stack replacement and pruning

Native stack apply and Git synchronization keep destructive work disabled by default.
Set permissions in the reviewed manifest. The policy is included in the plan fingerprint and operation identity.

```yaml
apiVersion: firecrab.dev/v1alpha1
kind: Stack
name: demo
lifecycle:
  replace: false
  prune: false
  deleteVmDisks: false
# Add the desired resource declarations here.
```

| Policy | Default | Effect when true |
| --- | --- | --- |
| `replace` | `false` | Delete and recreate exact owned resources whose immutable configuration changed |
| `prune` | `false` | Delete exact owned resources removed from the declaration |
| `deleteVmDisks` | `false` | Also remove the old standalone VM artifact directory during replacement or pruning |

`deleteVmDisks` requires `replace` or `prune`. It does not imply either permission.
With all resources removed, the manifest is valid only when `prune: true` is explicit.
An unreadable Git path, authentication error or invalid manifest never becomes an empty stack.

## Review before apply

```sh
firecrab stack validate --file stack.yaml
firecrab stack plan --file stack.yaml --json > plan.json
firecrab stack apply --file stack.yaml --expected-plan plan.json
```

Add `--allow-stop` when deleting or replacing an owned running VM or a service that owns replicas or a published route.
For Git sources, the source's `allowStop` supplies the same interruption permission.
It does not grant replacement, pruning or disk deletion by itself.
Paused or transitioning VMs require explicit lifecycle recovery before destruction.

`replace` entries remain visible in plans even when replacement is disabled; apply rejects them until the policy is enabled.
Removed declarations show `retain` by default and `delete` with pruning enabled.
Changing any policy or the disk identity after review invalidates the plan.
Automatic Git mode can perform these operations after a manifest commit enables the corresponding policy.
Detect-only checks remain read-only.

## Dependencies and ownership

Destruction runs in reverse dependency order: services, standalone VMs, then networks.
Creation runs in dependency order: networks, VMs, then services.
Replacing a network also replaces its declared owned VM and service dependents.
Every replacement reserves a fresh UUID before mutation. The old UUID remains owned until its resource is deleted.
Deletion completion and ownership release or transfer commit in one database transaction.
Existing resources are never adopted by name.

Preflight rejects network deletion when an external VM, lease, service or security group still references it.
It also rejects new network CIDRs that overlap resources outside the removal set.
VMs with attached external volumes must be detached explicitly before destruction; their data volumes are not deleted.
Builder, pool, queue, cluster-worker and service-owned replica VMs cannot be selected as standalone stack targets.
Services clean their own replicas through the existing service controller.
An active deployment, unfinished Deploy recovery, or pending replica repair blocks destructive work involving that service.

These checks do not lock out every manual API operation for the whole apply.
Later conflicts can stop a partially completed operation; its completed work and exact identities remain recorded.

## Standalone VM disk retention

By default, deleting a VM record through this workflow keeps its entire private artifact directory.
The operation's delete step records `resourceId` and `vmDisk` (`storageRoot`, `generation`).
Successful CLI output also reports retained disk identities.
The rootfs location is derived from those identities:

```text
STORAGE_PATH/vms/OLD_VM_UUID_WITHOUT_HYPHENS/d/GENERATION_UUID_WITHOUT_HYPHENS.ext4
```

The retained directory can include guest data, runtime logs and private VM keys. Keep its existing private permissions.
A null generation means no prepared rootfs was recorded; any partial artifacts are still retained.
Replacement creates a fresh stopped VM. It does not copy the old disk into the replacement or roll back application data.
Service replica disks remain disposable and follow normal service cleanup.

Export operation status before deleting empty stack registration metadata, which removes its operation history:

```sh
firecrab stack apply-status demo --id OPERATION_UUID > retained-disks.json
```

Registration deletion does not delete retained artifacts. Restore or archive them through an explicit host recovery procedure.
Direct resource API deletion keeps its existing hard-delete behavior; this retention policy applies to stack execution.

## Failures and recovery

The journal expands a replacement into an old-UUID `delete` step and a new-UUID `create` step.
The create step's `replacementFor` records its old identity.
An identical operation-ID retry returns recorded status and never repeats destruction.
API restart interrupts the stack operation. It does not replay a half-finished replacement.

Inspect each step, both UUIDs, current ownership and retained disk identities before explicit recovery.
If deletion completed but ownership transfer was not acknowledged, the old binding can point to a missing resource.
If transfer completed before creation, the new UUID remains bound but may be missing.
Repair only that exact missing binding after inspection, then review a fresh plan with a new operation ID.
For Git sources, acknowledge the failed operation through [source recovery](stack-git-source.md#manual-edits-and-recovery).

Completed deletes cannot be rolled back automatically. Keep retained data until recovery is verified.
See [native apply](stack-apply.md) for partial-success and timeout behavior.
