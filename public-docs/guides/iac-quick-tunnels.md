# Quick Tunnel ownership prerequisites

This is stage 2 of [#548](https://github.com/SteelCrab/firecrab/issues/548).
It provides ownership transfer and native mutation fences for existing independent
connectors. Complete declarative provisioning, public native operation journals,
readiness/timeout contracts and both Stack/Terraform execution remain pending.

## Exact connector identity

The `quick_tunnel` resource uses its exact non-nil connector UUID.
The original shared connector keeps `00000000-0000-0000-0000-000000000001`.
Ownership follows that identity through its legacy representation and the saved
connector list. A current public URL is observation data, never identity.

Use `/api/iac/owners/quick_tunnel/UUID` and the ordinary ownership transition API.
The privileged broker supplies presence, current ownership and a digest of the
saved target/name/listener projection. Credentials and remote token values do not
enter that observation. Root-owned address records identify generated children;
display names never grant or remove controller authority.

## Transfer and native mutation rules

The [host Ingress transfer protocol](iac-ingress.md) also applies to Quick Tunnel.
It reserves the exact target on the host until the API has durably mirrored the
original acknowledgement. Retry the same operation UUID after disconnection or
API restart. Terminal results remain immutable after subsequent owner changes.
Queued native unit jobs and unfinished local publication block new transfers.

Direct configuration changes, deletion, unit controls and whole-Pipe disable
respect the connector owner. Legacy shared tunnel controls use the same UUID
gate. A whole-manifest apply cannot resume or restart another workflow's managed
connector. Other routes and independently controlled peers retain their own
configuration. The API also rejects known managed connector changes before
contacting the helper; the privileged commit remains the final authority.

Generated Firecrab address and Sandbox preview connectors stay with their parent.
They cannot be adopted as independent Quick Tunnels. Root-owned completed and
pending address records both prove this relationship, including when publication
stops before the final address binding is saved.

## Interrupted parent publication

The address controller retains its generated child UUID, original target,
credential and protocol before local publication. A private operation ID selects
only that admitted intent. It is not accepted through native HTTP headers.
Retries with changed input fail. Original credential resolution is reused locally.
The independent updater leaves pending parent records unchanged.

Before replacing configuration or starting native jobs, publication durably marks
that local effects may have started. A proven earlier failure releases its
reservation and restores prior protocol metadata. An uncertain attempt keeps its
original child, input and Pipe reservation. Other native configuration/unit changes
wait for that operation. Retrying the original parent input resumes its private
operation; it does not create another connector. Deletion cannot abandon pending
publication. Full remote retirement, credential/delegation and managed workflow
recovery contracts remain part of the unfinished shared lifecycle work.

Host owner files use kind-qualified keys. The original ingress-only format is
upgraded without changing owners or receipts. Duplicate or inconsistent keys fail
closed. A schema field makes an older reader reject the new format instead of
silently ignoring its qualified owner entries. Use matching API/helper/CLI builds;
builds without these mutation gates do not support this ownership contract.

## Verification scope

Portable and Linux tests cover exact owner receipts, lost replies/API restart,
namespace migration, retained private input, pre-effect failure and uncertain
publication. `scripts/test-host-iac.py` exercises the actual Release root CLI in a
disposable Linux container, including legacy control and controller-child rejection.
Systemd observations and parent data in this protocol fixture are explicit fixtures.
It performs no real connector/network registration, Nginx/systemd operation, guest
boot, Stack apply or Terraform provider execution. These tests do not complete #548.
