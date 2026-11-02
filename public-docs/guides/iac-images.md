# Image ownership and durable native operations

An image management identity is `image/ALIAS`. Its owner controls alias selection
and the retained immutable boot versions. Each version includes the complete
kernel, rootfs, optional initrd and boot-argument SHA-256 pin. Changing the
selected version preserves historical files until an explicit unused-image
deletion. A persisted retirement prevents retained files from restoring a deleted
alias after restart.

The package install, OCI import, kernel update and image delete endpoints accept
`x-firecrab-operation-id` and `x-firecrab-endpoint-id`. Retain the same UUID when
retrying a lost acknowledgement. These headers identify a request; they do not
grant Stack or Terraform write permission. Existing local authorization and
remote mTLS continue to apply. An externally managed alias rejects ordinary
native changes.

Read the original operation through `/api/iac/native-operations/UUID`. Uncertain
work retains its alias and parent reservations. Recovery requires the observed
endpoint and execution generation. A terminal retry returns its original receipt
before inspecting mutable catalog inputs. Browser mutations retain only the
operation and endpoint identities in session storage. They never save request
bodies or registry credentials there.

Package installation freezes the archive SHA-256 and verifies the same opened
archive before extraction. OCI import freezes the manifest digest, original
naming reference, saved authentication configuration and selected kernel. The
private journal retains the original credentials; operation observations and
routine logs omit them. The kernel digest and boot arguments become immutable
before rootfs publication. A completed OCI rootfs belongs to the original
operation and is reused during recovery. Recovery does not rebuild different
bytes over a prepared pin.

A host publication lock coordinates image writers and ownership transfers.
Asynchronous producers capture their original authority before source work.
Late publication rejects a changed owner or generation, including a claim and
release that restores a native owner. The catalog uses its retained directory
descriptor. A changed root mapping blocks publication. Automatic Deploy cache
files remain controlled by their parent service.

Deletion checks durable VM, pool and service references. SQL creation fences
also cover creators that selected an image before deletion. Each original file
is hashed and checked for remaining references before removal through its parent
directory descriptor. Shared catalog artifacts and managed kernel cache files
are retained. `DELETE /api/images/ALIAS?retainArtifacts=true` removes registration
while retaining its files. Retention is part of the immutable request identity.

These are shared native prerequisites. Complete inventory-wide planning,
credential references, controller delegation, expanded Stack execution, the
Terraform provider and every-row live acceptance remain tracked in #548. Native
API or fixture tests do not establish completion of either IaC workflow.
