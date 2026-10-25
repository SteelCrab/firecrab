# Security groups

A security group is a reusable allow-only policy for one IPv4-only MicroNetwork.
Each VM can select one group.
New connections are denied unless a rule allows them.
Replies to established connections remain allowed.
An empty group denies all new peer connections.
Omitted or null `securityGroupId` keeps the existing network behavior.

## Dashboard

Open **Security groups** in the network menu.
Choose **New security group**, enter a name, and select an IPv4-only MicroNetwork.
The new group starts with no allow rules.
Add inbound or outbound rules with TCP or UDP, an inclusive destination port range, and a peer IPv4 CIDR, then choose **Save rules**.
For inbound rules the peer is the source; for outbound rules it is the destination.
Removing all rules restores default deny for new peer connections.

In **MicroService**, create or edit a configuration and select its **Security group**.
Choose **Save configuration** to store the selection.
The deployment screen selects that group's MicroNetwork automatically.
New replica VMs inherit the group before startup.
Configuration changes apply on the next deployment; stop and redeploy an existing service to use them.
Select **None — use network defaults** to remove the group from the saved service configuration.

Stop affected VMs and service deployments before saving rule changes.
Failed saves keep the draft rules in the editor.
The dashboard also supports deleting unreferenced groups.

Each group's connection diagram shows its MicroServices and their actual replica VMs.
Service boxes show the configured replica count; circles represent VMs from a deployment.
Services without VMs are labeled explicitly.
An **Inherited** label confirms that the VM record selects this group.
Unverified and mismatched selections have separate labels.
The diagram refreshes automatically without replacing unsaved rule edits.

## Create and attach

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/security-groups \
  -H 'Content-Type: application/json' \
  -d '{"name":"database","microNetworkId":"NETWORK_ID"}'

curl -fsS -X PUT http://127.0.0.1:5523/api/security-groups/GROUP_ID/rules \
  -H 'Content-Type: application/json' \
  -d '{"rules":[{"direction":"ingress","protocol":"tcp","portFrom":5432,"portTo":5432,"peerCidr":"172.30.0.0/24"}]}'
```

Pass the returned group UUID as `securityGroupId` when creating a VM.
The VM's `microNetworkId` must match the group's network.
For an inactive VM, use `PUT /api/vms/{id}` with the normal resource fields and `securityGroupId`.
Omit that field to preserve the selection; send null to detach it.

Rules use `ingress` or `egress`, `tcp` or `udp`, and an IPv4 peer CIDR.
Both ports are inclusive destination ports from 1 through 65535.
The API normalizes CIDRs and permits at most 256 rules per group.
When both peers have groups, the source egress and destination ingress must allow the connection.

## MicroService replicas

Select a group in the dashboard or set `securityGroupId` in the MicroService JSON configuration.
Deploy with the same network UUID, or reuse an existing matching `services` network.
The deployment snapshot captures the selection.
Each new replica receives it before guest startup.
Stop and redeploy creates new VMs with the same selection.
Missing, corrupt, or incompatible saved policies fail startup and recovery.
They never fall back to unrestricted access.

## Traffic and changes

ARP, DHCP, gateway DNS, and host-originated administration remain available.
Internal service HTTP is allowed only when a source egress rule permits TCP 80
to its own gateway. PipeIngress separately checks the destination service group
against the caller address and app port. Other new VM-to-host connections are denied.
Forwarded host ports still require an ingress rule for the original client CIDR and guest port.
Network `internetEnabled`, VM `egressPolicy`, and cross-network isolation still apply.

Stop affected MicroServices and VMs before replacing rules.
Wait for deployment cleanup to finish.
A VM with a remaining host runtime also blocks policy edits.
Changes that need an inactive VM return `409` while it is active.
Referenced groups cannot be deleted, including references from inactive VMs or services without replicas.
Delete groups before deleting their MicroNetwork.

## Host requirements

The helper needs nftables bridge connection tracking and the `conntrack` executable.
Upgrade the API and helper together; security groups require helper protocol version 3.
Linux installation attempts to install `conntrack` (`conntrack-tools` on RPM-based systems).
Missing support fails grouped VM startup before the guest runs.
The helper atomically installs bridge and routed policies and clears old tracked flows when leases are recycled.
Restarting the helper clears unknown tracked flows when groups are active; clients may need to reconnect.
It filters bridged traffic with `br_netfilter` enabled or disabled.

```sh
sudo nft list table inet firecrab_sg
sudo nft list table bridge firecrab_sg
```

This version provides dashboard and REST configuration on one host.
Live policy changes, IPv6, ICMP allow rules, group-to-group references, and multiple groups per VM are outside this scope.
See the [API contract](../reference/api.md#security-group) and [networking guide](networking.md).

## Verify in isolation

Unit tests use fake helpers and never change the host firewall.
The packet script requires a fresh network namespace and refuses existing host interfaces.
Use a disposable Linux QA machine with `iproute2`, `nftables`, `conntrack`, Python, and the `br_netfilter` and `nf_conntrack_bridge` kernel modules available.

```sh
FIRECRAB_SG_QA_FIXTURES="$PWD/target/sg-qa" \
  cargo test -p firecrab-helper export_security_group_packet_fixtures -- --ignored
```

Run `scripts/ci-qa-security-groups.py target/sg-qa --br-netfilter 0` in a new network and mount namespace with private `/proc` and `/sys` mounts.
Repeat with `--br-netfilter 1` in another fresh namespace.
Pass `--helper-tests /absolute/path/to/firecrab_helper-HASH` using the helper test binary from Cargo's build output to also check real connection cleanup.
The script covers TCP/UDP, both policy directions, replies, gateway routing, DNS/DHCP, host administration, cross-network isolation, DNAT, and atomic replacement failures.
