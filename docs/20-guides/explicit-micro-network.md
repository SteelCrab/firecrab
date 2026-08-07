# MicroNetwork

A MicroNetwork is a named IPv4 subnet for microVMs.
It is backed by a Linux bridge, DHCP, NAT, and firewall rules.

firecrab has no hidden default subnet.
Create a MicroNetwork before creating a VM.

## Create a network

```sh
curl -s -X POST http://127.0.0.1:3000/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.31.0.0/24","internetEnabled":true}'
```

The gateway is the first usable address in the subnet.
The API returns the network UUID.

Network CIDRs must not overlap.
Choose a private range that does not conflict with host routes.

## Create a VM in the network

Pass the returned UUID as `microNetworkId`.

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d '{
    "name":"web-1",
    "template":"alpine-3.24",
    "cpu":1,
    "ram":512,
    "diskGb":2,
    "microNetworkId":"<network-id>",
    "egressPolicy":"internet"
  }'
```

`microNetworkId` is required.
An unknown ID returns a validation error.

## Internet policy

`internetEnabled` controls the whole MicroNetwork.
It enables or disables NAT and forwarded traffic outside firecrab.

```sh
curl -s -X PATCH http://127.0.0.1:3000/api/micro-networks/<network-id> \
  -H 'Content-Type: application/json' \
  -d '{"internetEnabled":false}'
```

The VM field `egressPolicy` adds a per-VM policy.
Use `internet` or `isolated`.

DHCP and DNS on the network gateway remain available to isolated VMs.
Traffic between different MicroNetworks is blocked.

## Inspect a network

```sh
curl -s http://127.0.0.1:3000/api/micro-networks
curl -s http://127.0.0.1:3000/api/micro-networks/<network-id>
```

The detail response shows address use, bridge state, NAT, firewall state, and member VMs.

Inspect the host objects when deeper checks are needed.

```sh
ip -br link show type bridge
ip -br addr
sudo nft list table inet firecrab
```

MicroNetwork bridges start with `mnb`.
VM TAP interfaces start with `fct`.

## Delete a network

```sh
curl -i -X DELETE http://127.0.0.1:3000/api/micro-networks/<network-id>
```

Deletion returns `409 in_use` while a VM belongs to the network.
Delete or move the dependent VMs first.

## Recovery after restart

The database is the source of truth.
The API asks the helper to recreate missing bridges, DHCP state, and firewall rules.

Do not manage firecrab nftables rules by hand.
A later reconciliation can replace manual changes.

## Related documents

- [Architecture](../10-overview/architecture.md)
- [REST API](api.md)
- [Network helper](net-helper.md)
- [Troubleshooting](troubleshooting.md)
