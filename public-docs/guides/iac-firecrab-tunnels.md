# Firecrab fixed-address ownership

An independent saved fixed address uses `firecrab_tunnel` and its exact public
address label. Its physical host proof also pins the registry origin, protocol,
credential digest, original target and connector UUID. Transfer does not resolve a
new token or modify a registration. Credentials remain in the native private store.

The privileged owner ledger retains the original exclusively owned connector.
Deleting the mutable address file externally cannot make that connector available
for standalone adoption, control or configuration replacement. Borrowed connectors
stay independent. Unrelated routes remain editable. Restore changed original
address input or explicitly release its ownership before adopting a replacement.

The broker holds the address lock and Pipe lock in native order. Native deletion
checks the owner before remote retirement. Native saves check it before publishing
a child. The updater continues its existing observation/lease maintenance; it does
not receive permission to change the declared target or credential. API metadata
admission and Sandbox address claims exclude each other in the SQLite transaction.

After a lost acknowledgement, retain the original operation UUID. Retry that exact
request or read `/api/iac/owner-changes/UUID`. Confirmed terminal receipts remain
readable after the private address or manifest disappears. Release preserves the
address and connector; its reservation lasts until the API mirror is confirmed.

The public API cannot identify an opaque connection token locally. It rejects new
token-based native setup while a fixed-address ownership slot is occupied. Use the
declared workflow or explicitly release its owner before this native setup path.

## Verification scope

Tests cover lost replies/API restart, original credential drift, child preservation,
missing parent records and rejection before remote deletion. The actual root CLI
fixture uses explicit private parent data and fake systemd observations. It performs
no live address registration, remote retirement, systemd job or guest boot.

This adapter is an ownership prerequisite. Full original-operation remote
provisioning/retirement, credential references, Sandbox parent provenance across
all native transports, delegated managed execution and both complete Stack and
Terraform workflows remain part of #548. Its integration hold remains active.
