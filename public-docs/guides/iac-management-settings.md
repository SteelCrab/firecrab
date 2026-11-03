# Native management settings ownership

This is an unmerged stage 2 prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
It transfers ownership metadata for an existing macOS or Windows management installation.
Full planned configuration, managed execution, readiness and both IaC workflows remain required.
The `host_configuration` adapter is still unavailable.

## Physical installation

Run `firecrab iac native-identity` on the native host.
It reads the actual operating system's machine identity and the selected installation directory's filesystem identity.
It does not read the guest's platform display descriptor or start/reconfigure a management VM.
The installation UUID is stable for that machine and directory object.
A copied profile, replacement directory or different machine cannot silently become the original target.
Windows uses the containing volume and complete 128-bit file ID, including on ReFS.

The broker owns `iac-native/ownership.json` under the locally selected management home.
Its atomic journal retains owner generations, original operation hashes and unconfirmed reservations.
The API keeps a separate native physical binding; it does not reuse its Linux Pipe-host binding.
Both realms share API operation IDs, Stack limits and Terraform logical-address reservations.

## Authenticated channel

Use the existing offline Firecrab CA and host certificates.
The broker's certificate UUID must match `native-identity`'s `installationId`.
The API's native client certificate UUID must match `/api/iac/identity`'s persistent `endpointId`.
Issue these certificates with `firecrab ca issue-host-cert UUID --dir CA_DIRECTORY --san SERVER_NAME`.
Keep the CA private key on the existing offline issuing system.
Copy only each process's own certificate/private key and the CA certificate to its protected installation.

Start the native service under the installation's OS account:

```sh
firecrab iac native-serve \
  --bind NATIVE_ADDRESS:PORT \
  --endpoint-id API_ENDPOINT_UUID \
  --cert NATIVE_CERT_FILE --key NATIVE_KEY_FILE --ca CA_CERT_FILE
```

Configure these API environment references together before restarting that API:

| Variable | Value |
| --- | --- |
| `FIRECRAB_NATIVE_IAC_ADDRESS` | Native broker address and port reachable from this API |
| `FIRECRAB_NATIVE_IAC_SERVER_NAME` | DNS/IP SAN in the native server certificate |
| `FIRECRAB_NATIVE_IAC_INSTALLATION_ID` | Exact observed native installation UUID |
| `FIRECRAB_NATIVE_IAC_CERT` | API endpoint client certificate file |
| `FIRECRAB_NATIVE_IAC_KEY` | API endpoint private key file |
| `FIRECRAB_NATIVE_IAC_CA` | Existing private CA certificate file |

This channel has no plaintext fallback, browser CORS path or owner-header authorization.
Normal CA, certificate validity and hostname checks remain enabled.
Both the server installation UUID and client API UUID are checked before dispatch.
A sibling host certificate issued by the same CA is insufficient.
The frame limit is 16 KiB and each network exchange has a ten-second deadline.
The listener admits at most sixteen concurrent connections.
Transport failure leaves the original operation pending; it never proves that no change occurred.
No credential values enter manifests, ownership records or routine diagnostics.

## Observe, transfer and recover

`GET /api/iac/native/identity` discovers the authenticated native target and its implemented facets.
`GET /api/iac/identity` includes native capabilities only after that configured broker answers successfully.
Use `management_settings` and the exact native installation UUID with the shared ownership endpoints.
The standard `POST /api/iac/owners/transition` request retains the original operation UUID.
Recover it through `GET /api/iac/owner-changes/OPERATION_UUID` after a lost reply or API restart.

The broker snapshots saved/activated settings under the native installation, resource, controller and settings locks.
Active or uncertain configuration work prevents adoption.
Native settings saves, resource application, manual control, installation maintenance and removal check retained ownership.
The original successful result remains readable even if later configuration is malformed.
An unconfirmed release still blocks legacy changes.
Only acknowledgement of the durable API mirror releases that reservation.
Neither adoption nor release deletes a VM, resets disks or uninstalls the native service.

## Verification limits

Native filesystem/identity tests use temporary installation directories on the actual test OS.
Transport tests perform real TLS handshakes and reject wrong client UUIDs, server UUIDs and forged endpoint fields.
API recovery tests use actual TLS and SQLite with explicitly simulated native metadata replies.
Those fixtures verify lost acknowledgement/restart behavior; they do not boot or resize a real VM.
Full native runtime acceptance, host configuration, shared planning/execution/delegation and the Stack/Terraform adapters remain pending.
