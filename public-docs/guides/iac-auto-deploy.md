# Automatic deployment policy ownership


Use `service_auto_deploy` and the exact local service UUID, including disabled slots.
Transfer preserves policy, pause state, release history and watcher generation.
Pending image intent, unfinished service operations and controller-worker services block transfer.
Managed policies block direct configuration, pause/resume, implicit replacement by full service edits and parent deletion.
Unrelated service edits can retain the policy.

Registry observations and image-deployment admission carry private parent/policy authority snapshots.
SQLite checks both snapshots and watcher generation before saving observations or accepting a deployment.
Ownership handover does not implicitly grant the new owner an image writer.
The policy read reports suspension until explicit authorized reconfiguration.
Existing accepted deployment identities remain available; a new observation does not replay them.

This cohort supplies ownership and admission boundaries, not full declarative support.
Terraform-parent image delegation, shared lifecycle execution and both workflow declarations remain in stage 2–4 work.

Policies written before authority snapshots require explicit reconfiguration if ownership history exists. Reading or completing an older observation does not grant authority.
