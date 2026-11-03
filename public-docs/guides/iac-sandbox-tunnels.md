# Sandbox preview ownership

An independent saved preview binding uses `sandbox_tunnel` and its exact binding
UUID. Its project, task, attempt, runtime reference, fixed address, target and
successor history remain unchanged during ownership transfer. Adoption verifies
the original saved task and project. It never follows a later attempt or runtime.

Connecting, stopping, deleting, uncertain and unfinished handoff records cannot
change owner. Stable retained bindings can transfer without calling the helper,
publishing an address or changing credentials. Keep the original operation UUID
after a lost ownership reply. Its immutable receipt remains valid after later
transfers, while the current owner query reports the latest generation.

Public native publish, stop, delete, retarget, reconcile and verification paths
retain their existing caller/project checks and also obey ownership. Request
headers do not grant workflow execution authority. Persistence repeats the
authority check inside the mutation transaction, including handoff and late
native saves. Project deletion cannot remove a separately managed preview facet.
The existing constrained shutdown-evidence writer can update observations only;
it cannot change the binding or its address claim.

The fixed-address claim continues to exclude standalone adoption of its address.
These prerequisites do not implement a complete managed preview workflow. Durable
original-operation planning/execution, delegated parent authority across every
native transport, runtime/readiness observations, credential references, remote
retirement policy and complete Stack/Terraform execution remain required in #548.

## Verification scope

Tests use actual local SQLite and REST handlers with explicit task/runtime data.
They verify unchanged binding/task records, native and late-writer rejection,
original receipt replay after release, uncertain-state rejection and a changed
original attempt. Existing Sandbox lifecycle/shutdown tests continue to run.
These tests do not create a real guest, register an address, run a connector,
apply Stack or execute a Terraform provider. The integration hold remains active.
