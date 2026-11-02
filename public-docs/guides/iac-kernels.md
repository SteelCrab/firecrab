# Kernel ownership and recovery

Kernel management uses the exact host-local identity
`kernel/ARCHITECTURE:VERSION`. `stable` is an install selector. It resolves once
before admission and never becomes a different version during retry or recovery.
Ownership transfer verifies the installed architecture and content while holding
an exclusive filesystem lock. Transfer preserves bytes and the original cache.
Releasing metadata after external removal does not recreate the installation.

POST `/api/kernels/VERSION/install` and DELETE `/api/kernels/VERSION` accept
`x-firecrab-operation-id` and the optional `x-firecrab-endpoint-id` precondition.
The API retains the original definition before dispatch. It returns the operation
UUID in `x-firecrab-operation-id`. GET `/api/iac/native-operations/UUID` observes
its durable status. GET `/api/kernels/VERSION/install` retains the original
installation progress across API restarts. A completed operation receipt is an
acknowledgement of that operation; check the current kernel catalog for present
artifact readiness.

HTTP disconnection does not cancel accepted work. Retrying the same UUID and
request returns its original result, including after later removal or transfer.
A different request with that UUID is rejected. The journal retains the physical
image-root identity, host architecture, version, build recipe and any previously
verified image digest. The official source URL and SHA-256 become immutable
before source download or compilation. Public observations omit host paths.

An interrupted operation retains its target and parent reservations. Recover the
original UUID through `/api/iac/native-operations/UUID/recover`, using its observed
endpoint and execution-generation headers. An obsolete execution cannot publish
or acknowledge recovery. A changed root, unavailable original recipe or different
pinned image requires operator inspection; recovery cannot silently replace them.

A filesystem lock coordinates builds, removal, transfers and image borrowing.
An installation marker prevents another API database sharing the same physical
cache from claiming its writer namespace. Images may borrow verified
Terraform-owned kernels without permission to repair them. That lease remains
held until image registration. Removal rejects image references and retained VM
kernel hashes. Removing an image never uninstalls its separately managed kernel.

Package installation extracts only regular files and directories into private
staging. It publishes declared boot artifacts under content-addressed image paths.
Archive paths and links cannot overwrite the managed kernel cache, another alias
or an admitted VM's existing files. Reinstallation reuses verified immutable bytes.

Tests use tiny architecture-valid artifacts and injected journal failures. They
verify actual API, SQLite and filesystem behavior. They do not establish a real
kernel build or KVM boot. Expanded Stack execution, the Terraform provider and
complete live acceptance remain required by issue #548.
