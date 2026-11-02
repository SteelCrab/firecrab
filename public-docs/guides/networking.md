# Networking

A MicroNetwork provides an IPv4 subnet, bridge, DHCP, NAT, and firewall policy.
IPv6 is optional; every VM must select a network explicitly.

## Create

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.31.0.0/24","internetEnabled":true}'
```

Use a private CIDR that does not overlap host routes or another network.
The first usable address is the gateway.
Pass the returned UUID as VM `microNetworkId`.

## Address lifecycle

A VM keeps its IPv4/MAC lease across stop, crash, and API restart.
Its runtime TAP and firewall policy are removed when inactive.
Later starts reuse the reserved lease unless an orphaned host VM still holds it or the operator changes the VM's MicroNetwork.
Deleting the VM frees the lease; the next allocation uses the lowest free address.

## Change a VM's network

Stop the VM and open its Overview or Network tab in the dashboard.
Edit MicroNetwork, choose the destination by name and CIDR, and save.
The picker uses the same network catalog as VM creation.
If the current security group belongs to another network, explicitly choose a compatible group or None.
Firecrab does not remove that group automatically.

The API accepts `microNetworkId` in `PUT /api/vms/{id}` alongside the current resource values.

```sh
curl -fsS -X PUT http://127.0.0.1:5523/api/vms/VM_ID \
  -H 'Content-Type: application/json' \
  -d '{"cpu":1,"ram":512,"diskGb":2,"egressPolicy":"internet","microNetworkId":"DESTINATION_NETWORK_ID"}'
```

Omitting `microNetworkId` preserves the current network.
Sending the same UUID preserves the lease; null cannot detach a network.
A different UUID requires `created`, `stopped`, or `error` with no remaining VM process.
Service-owned replicas and pool-managed VMs must use their owning configuration instead.

Firecrab saves the VM settings and destination lease in one SQLite transaction.
An unknown network or exhausted destination returns `400`; allocation or storage failure preserves the previous settings and lease.
The old network loses the member and lease, and the destination gains both.
The next start attaches the TAP to the destination bridge and uses the new address.
Do not rely on retaining the previous IP or MAC address.

DHCP synchronization follows the commit.
If it fails, `503` states that the change was saved; read the VM to confirm its network.
Starting the VM retries synchronization.

## Internet policy

Both network `internetEnabled` and VM `egressPolicy: internet` must allow internet access.
An isolated VM keeps DHCP and gateway DNS.
Use [security groups](security-groups.md) to restrict connections within an IPv4-only MicroNetwork.

```sh
curl -fsS -X PATCH http://127.0.0.1:5523/api/micro-networks/NETWORK_ID \
  -H 'Content-Type: application/json' \
  -d '{"internetEnabled":false}'
```

`uplink` selects the host interface used for NAT.
Omit it or use null on create to select the default-route interface.
PATCH with a name pins the interface; PATCH with `""` resets it to automatic selection.
`GET /api/network` lists eligible interfaces.
Different networks can use different uplinks without extra routing tables.

The helper allows DHCP, DNS, and forwarding on each bridge in the enforcing host firewall.
It supports UFW, firewalld's trusted zone, iptables/ip6tables, and supported nftables host chains.
A later host drop rule can still block traffic; inspect the active firewall when diagnosis requires it.

## IPv6

Choose IPv6 when creating the network.
IPv4 remains required.

| Create fields | Result |
| --- | --- |
| No IPv6 fields | IPv4-only |
| `ipv6AddressMode` only | Generated unique-local `/64` |
| `ipv6Cidr` only | Selected `/64` with SLAAC |
| Both fields | Selected `/64` with SLAAC or DHCPv6 |

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab-v6","subnetCidr":"172.28.0.0/24","ipv6AddressMode":"slaac"}'
```

Unique-local prefixes use NAT66; global prefixes use direct routing and need an ISP-routed `/64`.
`2001:db8::/32` is documentation space and cannot provide public connectivity.
The response reports `ipv6Egress: nat66` or `direct`.

SLAAC derives an EUI-64 address from the VM MAC.
DHCPv6 uses a reserved address per VM.
Both store the address before startup and pin it in firewall policy.
Guests disable temporary addresses and use `addr_gen_mode=0`.
IPv4-only networks return null IPv6 fields.
Network internet policy applies to both families.

## Inspect

```sh
curl -fsS http://127.0.0.1:5523/api/micro-networks
curl -fsS http://127.0.0.1:5523/api/micro-networks/NETWORK_ID
ip -br link show type bridge
ip -br addr
sudo nft list table inet firecrab
```

Details include address use, bridge state, NAT, policy, and member VMs.
Bridge names start with `mnb`; VM TAPs start with `fct`.
The helper uses dnsmasq for DHCP and nftables for NAT, isolation, and anti-spoofing.
VMs on the same network can use their leased IPv4 addresses over its bridge.
Traffic between different MicroNetworks is blocked in both families.

## Delete and recover

Delete a network with `DELETE /api/micro-networks/{id}`.
Attached VMs, leases, or security groups return `409`; delete those resources first.

SQLite is the desired-state source.
Services recreate runtime networking after restart.
Do not edit Firecrab-owned nftables state manually; reconciliation can replace it.
See [network recovery retry](operations.md#retry-vm-network-recovery) for a running VM with failed networking.

## Related

- [API network fields](../reference/api.md#micronetwork)
- [Architecture](../reference/architecture.md)
- [Troubleshooting](troubleshooting.md)
