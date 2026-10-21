# RPM SQLite fixture

`fedora-42-basesystem.rpmhdr` is one `Packages.blob` row read from the
Fedora 42 OCI template in the Windows/WSL lab on 2026-10-04.
It describes `basesystem-11-22.fc42.noarch` (LicenseRef-Fedora-Public-Domain)
and contains package metadata only. The SQLite export starts with index/store
counts, without the eight-byte RPM file magic prefix.

SHA-256: `ba3854109504c15c87e329590d2989b3609d694d28a213b59ce7f6a6030e3de8`

See RPM's [header export implementation](https://github.com/rpm-software-management/rpm/blob/master/lib/header.cc).
