# Networking

A MicroNetwork is a named IPv4 subnet.

- Optional IPv6: send `ipv6AddressMode` or `ipv6Cidr`
- Per network: bridge, DHCP, NAT, firewall policy

## Contents

- [Create](#create)
- [Attach a VM](#attach-a-vm)
- [Internet policy](#internet-policy)
- [IPv6](#ipv6)
- [Host objects](#host-objects)
- [Inspect](#inspect)
- [Delete](#delete)
- [Recovery](#recovery)
- [Related](#related)

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

- Gateway: first usable address
- CIDRs must not overlap
- Use a private range that does not conflict with host routes
- This example is IPv4-only (no `ipv6Cidr`, no `ipv6AddressMode`)

## Attach a VM

- Pass the network UUID as `microNetworkId` on VM create
- Field is required
- Running VM: stored IPv4 and MAC lease
- TAP attached to that network's bridge

## Internet policy

- Network field `internetEnabled` controls NAT for the subnet

```sh
curl -s -X PATCH http://127.0.0.1:5523/api/micro-networks/<id> \
  -H 'Content-Type: application/json' \
  -d '{"internetEnabled":false}'
```

- `uplink`: host NIC for that NAT
- Omit or `null`: host default-route interface
- PATCH `""`: reset stored name to auto

```sh
curl -s -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"edge","subnetCidr":"172.32.0.0/24","uplink":"eth1"}'
```

- `GET /api/network` lists host interfaces for the picker
- Two internet-enabled networks can masquerade out different NICs
- Helper matches `oifname` on the existing postrouting chain
- No VRF, no extra route tables
- Helper opens DHCP (67/udp), DNS (53), and forward on each new bridge
- Helper talks to the host firewall that is actually enforcing policy:
  - UFW (Debian/Ubuntu)
  - firewalld `trusted` zone (Fedora/RHEL/openSUSE, no `--reload`)
  - iptables/ip6tables
  - nftables (`inet filter`, `ip filter`, NixOS `nixos-fw`)
- Firecrab's nft table does not hook INPUT — a later drop in that backend still wins unless the helper inserts there
- VM field `egressPolicy`: `internet` or `isolated`
- Both settings must allow internet traffic
- Isolated VMs keep DHCP and gateway DNS

## IPv6

- Create-time choice
- IPv4 stays mandatory — prefix is a second family, not a replacement
- Omit both `ipv6Cidr` and `ipv6AddressMode`: IPv4-only
- Send `ipv6AddressMode` without a prefix: unique-local `/64`
- Unique-local: not routable off the host → NAT66
- Global prefix: no translation; VMs hold public addresses
- `2001:db8::/32` is documentation space (RFC 3849) — replace with an ISP-routed `/64` before use

```sh
curl -s -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "lab-v6",
    "subnetCidr": "172.33.0.0/24",
    "ipv6Cidr": "2001:db8:1::/64",
    "ipv6AddressMode": "slaac"
  }'
```

- Prefix: `/64`, unique-local or global
- Egress follows scope — no separate switch
- Response `ipv6Egress`: `nat66` or `direct`
- `ipv6AddressMode`:
  - `slaac` — RA; guest derives EUI-64 from its MAC
  - `dhcpv6` — one reserved address per VM (same idea as the IPv4 lease)
- Both modes store the address before the VM starts
- Firewall pins it the way it pins the IPv4 lease
- Guest: `addr_gen_mode=0`, no temporary addresses
- Pre-dual-stack networks stay IPv4-only (`ipv6Cidr` is `null`)
- Internet-disabled: IPv6 egress dropped the same way as IPv4
- Traffic between two MicroNetworks: denied in both families

```sh
ip -6 -br addr show type bridge
sudo nft list table inet firecrab
```

## Host objects

- MicroNetwork bridges: `mnb*`
- VM TAPs: `fct*`

```sh
ip -br link show type bridge
ip -br addr
sudo nft list table inet firecrab
```

- Helper: dnsmasq for DHCP
- Helper: nftables for NAT, isolation, anti-spoofing
- Same MicroNetwork: VMs talk on leased IPv4 over that Linux bridge (no internet, no `internet` egress policy)
- Different MicroNetworks: blocked

## Inspect

```sh
curl -s http://127.0.0.1:5523/api/micro-networks
curl -s http://127.0.0.1:5523/api/micro-networks/<id>
```

- Detail: address use, bridge state, NAT, policy, member VMs

## Delete

```sh
curl -i -X DELETE http://127.0.0.1:5523/api/micro-networks/<id>
```

- `409` while a VM belongs to the network

## Recovery

- SQLite is the source of truth
- Services recreate missing runtime network state after restart
- Do not edit firecrab nftables rules by hand
- Reconciliation can replace manual changes

## Related

- [Architecture](architecture.md)
- [API](api.md)
- [Operations](operations.md)
- [Troubleshooting](troubleshooting.md)
