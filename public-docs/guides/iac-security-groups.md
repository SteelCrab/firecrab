# Shared security-group lifecycle

Rule changes reject an unconfirmed VM runtime when its storage mapping is
unavailable. The runtime probe never substitutes the default storage root for
that missing mapping. Restore the original mapping before retrying.
Missing or invalid service phase/replica metadata also cannot prove inactivity.
Native and shared policy deletion apply this check to retained deployment references.

The `security_group` adapter freezes the complete saved IPv4 policy for both
manager identities. It belongs to [#548 stage 2](../reference/iac.md). Full native
YAML, Terraform and automatic dependency orchestration remain separate stages.

## Configuration

Send a typed desired configuration to the [shared planning API](iac-resource-plans.md):

```json
{
  "kind": "security_group",
  "configuration": {
    "name": "application-policy",
    "microNetworkId": "11111111-1111-4111-8111-111111111111",
    "rules": [
      {
        "direction": "ingress",
        "protocol": "tcp",
        "portFrom": 443,
        "portTo": 443,
        "peerCidr": "10.20.0.0/16"
      }
    ]
  }
}
```

All ordinary policy limits apply. CIDRs are normalized to their network address.
Rule order is preserved. Empty rules keep default deny. Unknown fields, invalid
ports, invalid names, excessive rules and non-IPv4 networks fail before mutation.

The network is borrowed. Its UUID and address family are immutable in the native
API. The plan also pins its ownership generation. Internet/DNS metadata changes
do not replace this network identity. A new network binding requires a separate
replacement declaration; a group cannot move between networks in place.

## Execute and observe

The server saves the complete policy, exact ownership and original completion
receipt in one SQLite transaction. A matching declaration is a no-op. Name and
rule updates preserve the group UUID. Changed rules require all affected VMs and
service deployments to be inactive, as in the existing native API. The shared
handler also checks live process/file locks before applying rule changes.

The database rechecks active runtime states inside the mutation transaction.
VM starts record their active state before resolving policy rules. A competing
start therefore blocks rule replacement. The native policy writers use the same
ownership and runtime checks inside their transactions. A prior HTTP ownership
check cannot authorize a later write after management transfer.

Readiness means a validated saved policy available for the intended network.
Rules activate through the existing VM startup path. This adapter does not hot
replace another running VM's firewall or independently stop a borrowed workload.
The later workflow orchestrator must order permitted stops before policy changes.

Sandbox-generated groups remain owned by their environment. Direct adoption and
independent plans reject them. The environment controller uses an explicit parent
path that rechecks its exact environment UUID, generation and reserved child
inside the same transaction. A stale environment cannot modify a new generation.

## Delete and recover

Removal targets the exact owned UUID and rejects remaining VM or service
references. It deletes only that policy and its rules. The network, VMs and
application data remain intact. A completed receipt remains readable after API
restart, later edits or deletion. Repeating the original apply never recreates
the group or rewrites its rules.

Tests cover both manager identities, normalization, ordered rules, no-ops,
identity-preserving updates, borrowed-network retention, source-authority drift,
active VM conflicts, native writer fencing, HTTP execution and original receipt
recovery. These checks do not establish full YAML/provider or every-row live
acceptance.

The [clean Linux record](../assets/iac-548-five-plans-linux-evidence.json) pins
the exact source, executables, test totals and strict Clippy logs. It includes
the actual Sandbox parent-generation fixtures.
