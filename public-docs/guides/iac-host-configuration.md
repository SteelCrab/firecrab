# Native host configuration ownership

This is an unmerged stage 2 prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
It observes and transfers the ownership of an existing Linux API/helper installation.
It does not implement planned configuration changes, managed restart execution or the Stack/Terraform deployment adapters.
Those complete shared contracts remain required across the entire inventory.

## Installation proof

The native Linux broker reads its locally selected `CONFDIR` and `UNITDIR`.
No incoming request selects a directory, executable or shell command.
The resource identity is the exact UUID from `firecrab iac native-identity` on that Linux installation.
Its proof includes the machine identity and the configuration directory's original filesystem identity and creation time.
The systemd directory's canonical path and physical identity also enter the configuration proof.
Filesystems without a stable available directory creation time are rejected explicitly.

The configuration projection covers:

- The actual `api.env` file, including controller/worker settings and mTLS file references.
- The installed `firecrab-api.service` and `firecrab-helper.service` files.
- Every `.conf` entry in those units' drop-in directories.

The broker reads protected, bounded regular files with no symlink or hard-link aliases.
The private projection returns only a configuration hash, existence, busy status and ownership metadata.
It does not return environment values, private keys or certificate contents.
Unfinished service jobs and retained interrupted maintenance block adoption.
Ownership transfer preserves configuration bytes and running workloads.

## Authenticated setup

Reuse the existing Firecrab private CA and the [native broker certificate procedure](iac-management-settings.md).
The server certificate UUID identifies this actual Linux installation.
The client certificate UUID identifies the API database's persistent endpoint UUID.
The normal CA, hostname and expected UUID checks remain enabled.
Sibling certificates from the same CA cannot exercise the API endpoint's authority.

Run the native broker as the Linux installation owner, normally root:

```sh
sudo env CONFDIR=/etc/firecrab UNITDIR=/etc/systemd/system \
  firecrab iac native-serve --bind 127.0.0.1:5525 \
  --endpoint-id API_ENDPOINT_UUID \
  --cert NATIVE_CERT_FILE --key NATIVE_KEY_FILE --ca CA_CERT_FILE
```

For a sole native broker, the existing six `FIRECRAB_NATIVE_IAC_` transport references are sufficient.
For an API that also manages its outer macOS/Windows installation, configure the Linux service broker separately:

| Environment reference | Value |
| --- | --- |
| `FIRECRAB_HOST_CONFIGURATION_IAC_ADDRESS` | Linux broker address and port reachable from the API |
| `FIRECRAB_HOST_CONFIGURATION_IAC_SERVER_NAME` | DNS/IP SAN in that server certificate |
| `FIRECRAB_HOST_CONFIGURATION_IAC_INSTALLATION_ID` | Exact observed Linux installation UUID |
| `FIRECRAB_HOST_CONFIGURATION_IAC_CERT` | API endpoint client certificate file |
| `FIRECRAB_HOST_CONFIGURATION_IAC_KEY` | API endpoint client private key file |
| `FIRECRAB_HOST_CONFIGURATION_IAC_CA` | Existing private CA certificate file |

All six references for a channel must be set together.
They stay outside infrastructure manifests and routine state.
The outer management target and Linux configuration target retain separate physical bindings.
They share API operation IDs, Stack limits and Terraform logical-address reservations.
An acknowledged retry still returns its original result after API restart or a broker outage.

`GET /api/iac/native/host-configuration/identity` discovers the selected Linux configuration broker.
`GET /api/iac/identity` reports only successfully authenticated native capabilities and lists their `nativeTargets`.
Use `host_configuration` and the exact native installation UUID with the standard ownership endpoints.
Recover the original operation through `GET /api/iac/owner-changes/OPERATION_UUID`.

## Legacy maintenance and recovery

The normal Linux service CLI keeps a root-side lease across mutating service operations.
The standalone installer obtains the same lease before configuration changes and service removal.
A compatible verified payload CLI supplies the lease during a new bootstrap.
An existing native ownership journal with an incompatible installer is rejected.
An IaC owner or an unconfirmed release blocks legacy maintenance before any systemd effect.

The lease persists its original UUID, action and input hash before acknowledging admission.
The installer verifies any inherited parent step against that retained request.
Only the exact original request can resume; another UUID or changed input cannot replace unresolved maintenance.
EOF, process interruption and failure leave an explicit pending record in the protected native journal.
They never imply that configuration changes completed or had no effect.
After checking the original interrupted action, the root operator can resume its retained local maintenance handshake and explicitly complete that same request.
This local maintenance mechanism grants no permission to execute changes for a managed Stack/Terraform owner.
The private planned workflow execution and recovery contract remains pending.

## Verification limits

The dedicated Linux container executes the actual Release CLI, native filesystem journal and native TLS server.
It verifies interrupted maintenance, original-request recovery, native ownership, CLI/installer refusal, confirmed release and wrong-principal rejection.
Systemd observations are explicit fixtures; no service or guest is actually started or stopped.
API dual-target tests use real TLS and SQLite with explicitly simulated native metadata replies.
Neither fixture represents a real Stack apply, Terraform provider execution or complete live service acceptance.

The TLS fixture uses Python's unchanged default certificate verification.
[Python 3.13 enables strict X.509 checks by default](https://docs.python.org/3.13/library/ssl.html#ssl.create_default_context).
New Firecrab host certificates include the issuing CA's Authority Key Identifier.
The CA, hostname and endpoint UUID checks remain intact.
