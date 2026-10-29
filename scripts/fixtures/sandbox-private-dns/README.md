# DNS posture fixture

These Python standard-library probes produced the dnsmasq 2.91 Linux fixture
record in `public-docs/development/sandbox-environments-validation.md`. They
are namespace/process tests, not MicroVM or provider validation. They contain
no credentials and do not access an account database.

The server uses a local counting synthetic upstream at `127.0.0.9:15353`.
Its returned IPv4/IPv6 addresses are documentation addresses. It never queries
an external DNS service. Use `server.py groups` for the accepted two-daemon
policy. The `interface` and `address` modes reproduce rejected authoritative
prototypes that prevented private DHCP; they are negative controls.

Use two disposable Linux containers on two owned **internal** Docker networks:

| Container | Legacy addresses | Private addresses |
| --- | --- | --- |
| `firecrab-414-dns-server` | `10.241.41.2`, `fd12:414:41::2` | `10.241.42.2`, `fd12:414:42::2` |
| `firecrab-414-dns-client` | `10.241.41.3`, `fd12:414:41::3` | `10.241.42.3`, `fd12:414:42::3` |

The network names in `topology.json` are `firecrab-414-dns-legacy-20261026` and
`firecrab-414-dns-private-20261026`. Capture Docker's observed network membership
and MAC addresses before starting the server in this shape:

```json
{"containers":{"firecrab-414-dns-client":{"networks":{
  "firecrab-414-dns-legacy-20261026":{"MacAddress":"OBSERVED_LEGACY_MAC"},
  "firecrab-414-dns-private-20261026":{"MacAddress":"OBSERVED_PRIVATE_MAC"}
}}}}
```

Mount a fresh owned result directory at `/qa` in both containers. Put the
scripts and `topology.json` there. Supply a verified public dnsmasq runtime at
`/qa/runtime/usr/sbin/dnsmasq`, its aarch64 loader at
`/qa/runtime/lib/ld-linux-aarch64.so.1`, and its shared libraries at
`/qa/runtime/lib/aarch64-linux-gnu`. The original test used the already present
QA binary and its public shared libraries. Do not copy private configuration,
keys, tokens or databases into this directory. A different architecture needs
a corresponding explicit loader path in `server.py`.

Start the server with `python3 /qa/server.py groups`, wait for its
`server-ready.json`, then run `python3 /qa/client.py` in the client. Server
network administration capability is needed only inside its disposable
namespace. Limit each container to 0.5 CPU and 256 MiB. The DHCP probe performs
Discover/Offer/Request/ACK using the assigned fixture address and unicast server
destination; it does not claim to test initial broadcast delivery in a VM.

The client warms legacy DNS cache, verifies private external answers and
upstream counts, verifies local service and DHCP names, then tests all 65536
QTYPE values over each of IPv4/IPv6 UDP/TCP. TCP sessions reconnect every 32
queries to stay below dnsmasq's per-session cap. It writes `result.json` even
on failure and raises an error for a failed assertion.

Capture `result.json`, logs, binary checksum and topology. Remove only the exact
owned container/network IDs after checking ownership and empty network
endpoints. Preserve failed runs separately. These scripts do not create or
remove Docker resources automatically, and do not modify a running QA helper.
