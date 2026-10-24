# Mutual TLS for remote hosts

The default API remains HTTP on `127.0.0.1:5523` without authentication.
Keep that listener on trusted loopback, or reach it through an SSH forward.
Binding to any non-loopback address requires real mutual TLS (mTLS).
The old `FIRECRAB_AUTHENTICATION_ENABLED` and `FIRECRAB_TLS_ENABLED` flags cannot
permit a plaintext remote listener.

mTLS verifies both sides of a connection. The only trust root is one private,
self-signed CA certificate. Public WebPKI roots are not used by the authenticated
API client. Missing, expired, foreign-CA, or invalid-identity client certificates
are rejected before HTTP requests reach the API.

## Issue certificates

Run these commands on the issuing machine. They do not contact an API.

```sh
firecrab ca init --dir ./pki
firecrab ca issue-host-cert 11111111-1111-4111-8111-111111111111 \
  --dir ./pki --san 192.0.2.10 --san firecrab.example.test
firecrab ca issue-host-cert 22222222-2222-4222-8222-222222222222 --dir ./pki
```

The first certificate identifies an API host. Its DNS/IP SANs must match the
address used by clients. The second identifies a caller, such as a future
controller. Each certificate contains its UUID as the CN and as
`urn:firecrab:host:UUID` in a URI SAN. Those identities must match.
Certificates support server and client authentication.

Without `--dir`, the PKI directory is `~/.firecrab/pki`, or
`FIRECRAB_CONFIG_DIR/pki`. `ca init` creates `ca.crt` and `ca.key`.
Host files are `hosts/HOST_UUID/host.crt` and `host.key`.
Generated directories use mode `0700`; files use `0600` on Unix.
Existing output directories are refused. Partial failures never overwrite an
existing CA or host key. Inspect incomplete output before retrying.

Keep `ca.key` on the issuing machine. Copy only the host's leaf certificate,
private key, and `ca.crt` to that host. A caller receives its own leaf/key and
the same `ca.crt`. Any valid caller certificate from this CA grants API management
access in this phase; per-host authorization and revocation are separate work.

## Configure the API

Install the three files as regular files with mode `0600`, readable by the API
service user. Symlinks and group/world-readable files are rejected on Unix.
On Windows, restrict access to the account using the key through file ACLs.

Set these variables in the API service environment, then restart that service:

```sh
FIRECRAB_BIND_ADDR=0.0.0.0:5523
FIRECRAB_TLS_CERT=/etc/firecrab/tls/host.crt
FIRECRAB_TLS_KEY=/etc/firecrab/tls/host.key
FIRECRAB_TLS_CA=/etc/firecrab/tls/ca.crt
```

All three TLS variables must be set together. A bad chain, mismatched key,
expired/not-yet-valid certificate, or unsafe file mode fails startup before VM
recovery or scheduled work begins. Supplying these files also enables mTLS on a
loopback listener, which is useful for local verification.

Authenticated handlers can obtain `ConnectInfo<tls::AuthenticatedHost>`.
Its host UUID comes from the verified connection. HTTP headers and request
bodies cannot replace it. This provides authenticated transport; it does not
add host registration, placement, or controller high availability.

## Connect a client

Use the caller's files, not the API host's private key:

```sh
export FIRECRAB_TLS_CERT=./pki/hosts/22222222-2222-4222-8222-222222222222/host.crt
export FIRECRAB_TLS_KEY=./pki/hosts/22222222-2222-4222-8222-222222222222/host.key
export FIRECRAB_TLS_CA=./pki/ca.crt
firecrab host add remote https://192.0.2.10:5523
firecrab --host remote vm list
firecrab --host remote vm console VM_UUID
```

HTTPS and WSS use the same private CA and caller identity. Certificate loading
errors are reported without panicking. Plain HTTP/WS loopback clients do not
load these TLS files. Host profiles continue to store only endpoint URLs.

A direct request can use:

```sh
curl --cacert "$FIRECRAB_TLS_CA" --cert "$FIRECRAB_TLS_CERT" \
  --key "$FIRECRAB_TLS_KEY" https://192.0.2.10:5523/api/host
```

## Expiration and renewal

Certificates default to 365 days. `--days` accepts 1–3650 days, bounded by the
CA expiration. Linux `firecrab doctor` reads the same TLS environment and checks
the leaf, CA, chain, key, identity, and permissions. It reports failure for
invalid material or expiration within 30 days. Unconfigured TLS is skipped.

To reissue the same host UUID, archive its generated `hosts/HOST_UUID` directory
on the issuing machine, then issue again. Replace the separately installed leaf
and key together and restart the API. Existing connections do not reload changed
certificate files. Rotate the CA deliberately across servers and callers when
its trust must change; no public CA or automatic revocation service is added.
