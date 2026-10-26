# Bounded HTTP and WebSocket preview fixture

This fixture prepares the later, explicitly authorized preview check. It does not
register an account or token, open an authentication page, create an Environment,
change a seed image/script, publish a port or contact a public test domain.
The user must first supply the registered test domain and saved connection name.
Root then confirms the exact owned Environment, generation, Backend VM/runtime,
address and publication scope. Public exposure is limited to this fixture's HTTP
and WebSocket endpoints.

Complete the two strict-DNS Environment seed/recovery checks before installing an
operator overlay on one exact Backend at port 8080. Preserve their original seed
pins, scripts, ownership ledgers and recovery receipts. The overlay is a separate
root-owned file. It must not become part of a shared template or model workspace.

## Fixed surface and limits

`scripts/qa-sandbox-http-ws.py` uses only the Python standard library. It supports
Linux Alpine Python without pip, a framework, an engine socket or an external
service. It reads only its own `/proc/self/status` privilege fields. It has no
file/directory, process, environment, account, proxy, command or configuration
endpoint. Paths and query strings do not select files. Request data never enters
logs. Normal HTTP responses use `application/octet-stream` or fixed JSON,
`Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.

| Route | Result |
| --- | --- |
| `GET /health` | Fixed 45-byte JSON, including its final newline |
| `POST /echo` | Exact request bytes; explicit Content-Length, at most 4096 bytes |
| `GET /ws` | Version-13 WebSocket text/binary echo, fragmentation, ping/pong and close |
| Other method/path/query | Empty 404; no file or account lookup |

The health body is exactly:

```json
{"fixture":"firecrab-414-http-ws","ok":true}
```

Its SHA-256, including the newline, is
`43f08c592ded2e57249d911409464a97e5e25122c2ec90672297e841886e4da7`.
Use that digest for the Sandbox HTTP content check. It is fixture content, not
evidence of an application or a model completing work.

The HTTP header block is at most 4096 bytes. Duplicate/folded headers, ambiguous
lengths, transfer encoding and unsupported handshake/version/key are rejected.
The fixture accepts no compression extension or WebSocket subprotocol. It can
receive extension offers but does not negotiate them. Client frames must be
masked, minimally encoded and use known opcodes with zero reserved bits.
Fragmented messages share the same 4096-byte aggregate limit. Control frames are
final and at most 125 bytes. Text and close reasons must be valid UTF-8. Invalid
protocol/text/oversize frames receive 1002/1007/1009. A TCP reset after an error
close can occur when the invalid request still has unread bytes.

There are at most four workers, 64 frames and 16 echoed messages per connection.
The latter two budgets end with close code 1008. One absolute 10-second deadline
includes headers and every WebSocket frame; inactivity is at most two seconds.
Sending occasional bytes cannot extend the absolute deadline. The process has
64 file descriptors, 128 MiB address space, 15 CPU seconds, no core dumps and a
default 300-second lifetime. `--lifetime-seconds` accepts only 1..900. The process
refuses root/effective-root, NoNewPrivs other than 1 or nonzero effective Linux
capabilities. Its file-size limit is zero; use a pipe or `/dev/null` for its
stdout/stderr, not a guest regular log file.

The default listener is `127.0.0.1:8080`. `0.0.0.0` requires an explicit operator
argument after target approval. Browser WebSocket Origin must exactly equal
`--origin`; if no origin was configured, requests with Origin are denied. Native
QA clients without Origin are permitted. No permissive CORS header is returned.

The wire rules follow [RFC 6455](https://www.rfc-editor.org/rfc/rfc6455.html),
especially sections 4.2, 5.2–5.5 and 7.4. Accepted close codes also follow the
[IANA WebSocket registry](https://www.iana.org/assignments/websocket).

## Operator installation and launch

Do not run these deployment steps until Root has recorded the exact approved
domain, saved connection, owned VM and current runtime. Keep the existing strict
network/egress policy. Do not install packages over that network or read/copy any
provider, SSH or controller private key. Reference the already pinned management
transport only. If Python is absent, prepare a verified offline operator bundle;
do not weaken the network policy or change the seed pin.

Copy only the fixture script to `/opt/firecrab-qa/http-ws-414/server.py` on that
owned Backend. The parent directory must be root-owned 0755 and the script
root-owned 0444. Record the actual uploaded byte count and SHA-256. Confirm port
8080 is unused and numeric UID/GID 65041 does not own another service/process.
It is above the Sandbox helper's normal 1000..59999 allocation range. Do not
reuse an active model task's UID or alter a borrowed VM's account state.

Run through the existing trusted UID helper (or operator-installed util-linux
`setpriv` with the same flags). Python isolated mode ignores user site packages,
the current directory and Python environment configuration:

```sh
cd /
exec /usr/local/sbin/firecrab-console-resize \
  --reuid 65041 --regid 65041 --clear-groups --no-new-privs \
  /usr/bin/env -i HOME=/nonexistent PATH=/usr/local/bin:/usr/bin:/bin \
  /usr/bin/python3 -I -B /opt/firecrab-qa/http-ws-414/server.py \
  --host 0.0.0.0 --port 8080 --lifetime-seconds 300 \
  --origin 'https://APPROVED_TEST_DOMAIN'
```

`APPROVED_TEST_DOMAIN` is an instruction placeholder. It must be replaced by the
user's exact domain. This command supplies no token, connection secret or project
file. Keep the foreground SSH process or capture its startup line through a
bounded pipe into a mode-0600 operator evidence file. For a background launch,
use `/dev/null` output and record the exact PID, boot ID and start ticks from the
operator parent. Stop only that verified identity. Do not add a persistent boot
service or replay it through a completed model task.

Verify UID, effective UID, empty supplementary groups, CapEff=0 and NoNewPrivs=1
on that exact process. Then check `/health` on the owned VM's private address.
Only after this succeeds should Root bind the existing registered address with
the Sandbox REST/CLI/MCP preview contract. The declared target is that exact
Backend runtime and port 8080. Do not use raw Pipe mutation to bypass its claim.

## Separate public evidence

Record actual HTTP status, no-store header, exact 45 bytes and digest through the
approved fixed HTTPS URL. Record an actual WebSocket 101 accept hash, fresh
client masks, text and binary echo bytes, ping/pong and a normal close handshake.
Use only public test payloads, each at most 4096 bytes. A helper heartbeat or a
fixture test does not prove public HTTPS, WebSocket frames, shutdown or remote
retirement. Preserve failed and uncertain responses before selecting the next
bounded operation.

After validation, stop the Sandbox binding, verify its exact connector is
inactive, and stop the operator fixture by its recorded process identity.
Retain the fixed-address and remote-retirement receipts separately. Removing
local configuration does not prove remote token retirement or public shutdown.
Do not stop or delete the strict Environments until Root approves their normal
owned cleanup path and all required seed/recovery artifacts are exported.

## Local Linux fixture receipt

Use `scripts/test-qa-sandbox-http-ws.py`; it binds only ephemeral loopback ports.
The full regression requires Linux, a non-root UID, CapEff=0 and NoNewPrivs=1.
The macOS protocol-only subset is `python3 -B scripts/test-qa-sandbox-http-ws.py
EchoTests`; it is not the Linux privilege/lifetime acceptance check.

The local Docker context was inspected without changing its global setting:
`desktop-linux`, Unix socket `/Users/pista/.docker/run/docker.sock`, with no
DOCKER_HOST/DOCKER_CONTEXT override. The official public image was pulled only
for dependency preparation. The resolved Linux arm64 image is
`python@sha256:b658b05ce9c4eb97ec6fc19424818a1f31993659c2b505a532f470e83807815d`
(78,865,803 bytes). Host free disk before the pull was about 23.4 GiB, above the
3-GiB floor. No shared images, volumes or build caches were removed.

```sh
docker --context desktop-linux run --rm \
  --name firecrab-414-entrypoints-http-ws-qa \
  --network none --cpus 1 --memory 256m --pids-limit 32 --read-only \
  --user 65534:65534 --cap-drop ALL --security-opt no-new-privileges \
  --mount type=bind,src="$PWD",dst=/workspace,readonly --workdir /workspace \
  python@sha256:b658b05ce9c4eb97ec6fc19424818a1f31993659c2b505a532f470e83807815d \
  python3 -B scripts/test-qa-sandbox-http-ws.py
```

This exact Linux arm64 invocation passed all 14 tests in 2.752 seconds. It covers
the 12 HTTP/WebSocket behavior groups plus actual isolated-process startup,
privilege validation, bounded lifetime and invalid arguments. The macOS
loopback-only subset passed its 12 groups separately. Two additional network-none
Linux invocations refused root UID and NoNewPrivs=0 with exit 2 before printing a
startup address. All three disposable containers were removed by `--rm`.

This command publishes no host port and has no external network. It uses no
Cargo cache, account state, live Environment or seed artifact. Host free disk
after validation remained about 23.8 GiB. The image stays available as a public
QA dependency; no shared cache was cleaned. A private mode-0600 receipt and
script byte hashes are retained at
`/private/tmp/firecrab-414-http-ws-qa/receipt.json`. These fixture results do not
claim a live guest, public HTTPS/WebSocket or provider/model acceptance check.
