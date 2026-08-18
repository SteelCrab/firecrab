# Networking

A MicroNetwork is a named IPv4 subnet.
It gives VMs a bridge, DHCP, NAT, and firewall policy.

## Create

```sh
curl -s -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "lab",
    "subnetCidr": "172.31.0.0/24",
    "internetEnabled": true
  }'
```

The gateway is the first usable address.
CIDRs must not overlap.

Choose a private range that does not conflict with host routes.

## Attach a VM

Pass the network UUID as `microNetworkId` when creating a VM.
The field is required.

Each running VM gets a stored IPv4 and MAC lease.
Its TAP interface is attached to the network bridge.

## Internet policy

The network field `internetEnabled` controls NAT for the subnet.

```sh
curl -s -X PATCH http://127.0.0.1:5523/api/micro-networks/<id> \
  -H 'Content-Type: application/json' \
  -d '{"internetEnabled":false}'
```

Optional `uplink` chooses the host NIC used for that NAT.
Omit it, or send `null`, to use the host default-route interface.
An empty string on PATCH resets the stored name back to that auto default.

```sh
curl -s -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"edge","subnetCidr":"172.32.0.0/24","uplink":"eth1"}'
```

`GET /api/network` lists host interfaces for the picker.
Two internet-enabled networks can masquerade out different NICs.
The helper matches `oifname` on the existing postrouting chain.
It does not install a VRF or extra route tables.
The helper opens DHCP (67/udp), DNS (53), and forward on each new bridge.
It talks to the host firewall that is actually enforcing policy: UFW
(Debian/Ubuntu), firewalld `trusted` zone (Fedora/RHEL/openSUSE, no
`--reload`), iptables/ip6tables, or nftables (`inet filter`, `ip filter`,
NixOS `nixos-fw`). Firecrab's own nft table does not hook INPUT, so a later
drop in that backend still wins unless the helper inserts there.

The VM field `egressPolicy` controls one VM.
Its values are `internet` and `isolated`.

Both settings must allow internet traffic.
DHCP and gateway DNS remain available to isolated VMs.

## Host objects

MicroNetwork bridge names start with `mnb`.
VM TAP names start with `fct`.

```sh
ip -br link show type bridge
ip -br addr
sudo nft list table inet firecrab
```

The helper runs dnsmasq for DHCP.
It uses nftables for NAT, isolation, and anti-spoofing.

VMs attached to the same MicroNetwork can communicate directly over their
leased IPv4 addresses. This traffic stays on that network's Linux bridge and
does not require internet access or an `internet` VM egress policy.

Traffic between different MicroNetworks is blocked.

## Inspect

```sh
curl -s http://127.0.0.1:5523/api/micro-networks
curl -s http://127.0.0.1:5523/api/micro-networks/<id>
```

The detail response shows address use, bridge state, NAT, policy, and member VMs.

## Delete

```sh
curl -i -X DELETE http://127.0.0.1:5523/api/micro-networks/<id>
```

Deletion returns `409` while a VM belongs to the network.

## Recovery

SQLite is the source of truth.
The services recreate missing runtime network state after restart.

Do not edit firecrab nftables rules by hand.
Reconciliation can replace manual changes.

## Related

- [Architecture](architecture.md)
- [API](api.md)
- [Operations](operations.md)
- [Troubleshooting](troubleshooting.md)
