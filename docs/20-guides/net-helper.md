# Network helper

`firecrab-net-helper` performs privileged host network operations.
The API stays unprivileged and talks to the helper through a Unix socket.

## Responsibilities

The helper can:

- Create and remove MicroNetwork bridges.
- Create and remove VM TAP interfaces.
- Apply nftables NAT, isolation, and anti-spoofing rules.
- Run and reload dnsmasq DHCP state.
- Enable IPv4 forwarding.

The helper does not manage VM records or Firecracker processes.

## Trust boundary

The protocol accepts typed UUIDs, IP addresses, and policy IDs.
The helper derives bridge, TAP, and hostname values itself.

The socket permission is not the only check.
The helper also checks the peer UID with `SO_PEERCRED`.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `FIRECRAB_NET_HELPER_SOCK` | `/run/firecrab/net-helper.sock` | Unix socket path |
| `FIRECRAB_NET_HELPER_ALLOWED_UID` | Helper UID only | Extra API service UID |
| `FIRECRAB_BRIDGE_MTU` | Detected uplink MTU | MTU for firecrab bridges |

The API and helper must use the same socket path.

## Development run

Use the provided script for local work.

```sh
./scripts/dev-net-helper.sh
```

Or build and run the helper directly.

```sh
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" \
  FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper
```

Start the API from another terminal.

```sh
cargo run -p firecrab-api
```

## Installed service

`install.sh` installs two systemd units.
It configures the API user as an allowed helper peer.

```sh
systemctl status firecrab-net-helper firecrab-api
ls -l /run/firecrab/net-helper.sock
journalctl -u firecrab-net-helper -f
```

Start the helper before the API.
VM start fails when the helper socket is unavailable.

## Protocol operations

The current protocol includes these operations:

- Ensure or remove a MicroNetwork bridge.
- Rebuild the owned firewall rules.
- Create or delete a TAP interface.
- Apply or remove one VM policy.
- Replace the full DHCP lease snapshot.

Requests and responses use length-prefixed JSON frames.
The protocol has an explicit version.

## Related documents

- [Architecture](../10-overview/architecture.md)
- [MicroNetwork](explicit-micro-network.md)
- [Troubleshooting](troubleshooting.md)
