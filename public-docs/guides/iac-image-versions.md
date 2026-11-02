# Immutable image versions

Image aliases select immutable versions. An alias/version pair keeps its kernel,
rootfs, optional initrd and boot-argument hashes. Reinstalling identical content
may restore verified physical files. Different boot inputs require a new version.
A registration that changes an existing version is rejected before the live alias
or persistent catalog changes.

Selecting a new version retains earlier versions and their files. The runtime
catalog stores each version's locators and verified boot hashes. Older VM and
Stack pins remain addressable after API restart. Previously built-in or pre-seeded
versions are recorded before alias replacement, so a later software catalog does
not erase those dependencies. Legacy catalog entries remain readable.

Startup verifies recorded boot hashes. Missing or altered current artifacts do
not silently select an older version. Earlier valid versions stay available by
their exact pin. Image readiness must use the selected version, not the presence
of any historical image.

A corrupt catalog cannot become an empty write baseline. Registration and removal
fail without forgetting the live selection or deleting its artifacts. Catalog
publication uses a unique, synced temporary file and atomic rename. Deploy cache
collection preserves boot hashes for unrelated images.

Explicit image deletion still requires the image to be unused. It removes the
selected alias and its historical versions, and returns only artifact paths that
no other registered version references. Borrowed catalog artifacts and separately
managed kernel installations remain protected. Historical image versions also
prevent their managed kernels from being removed.

Package installation uses private extraction and content-addressed artifact
publication. It cannot overwrite another installed version's files. Existing
VM disks remain distinct from their source image artifacts.

These rules are shared lifecycle prerequisites for issue #548. They do not add
an image ownership adapter or complete durable image operations, expanded Stack,
the Terraform provider or every-row live acceptance. Those requirements remain
open and retain the issue's merge hold.
