# microManager release manifest

[`packaging/micromanager.toml`](../packaging/micromanager.toml) defines the release
inputs for macOS and Windows. The CLI embeds this file at build time. Maintainers
change it and build a new CLI to publish a different set of installation inputs.

The manifest contains the Debian version and suite, Firecrab and Firecracker
versions, platform images, Debian kernels, and each download's filename, HTTPS
URL, and checksum.
macOS uses ARM64 assets. Windows selects x86_64 or ARM64 assets.

## Existing installations

Updating the CLI does not migrate an existing installation. `service start`,
`stop`, `status`, `dev`, and `repair` use its existing files and provisioning
markers. They do not create a manifest or replace its kernel or OS.
`service dev` still runs installation if no management VM is installed.

An installation without `<managed-home>/micromanager.toml` uses the legacy
download pins for validation and offline OS repair.

## Command behavior

| Command | Installation behavior |
| --- | --- |
| First `service install` | Install directly from the bundled TOML. |
| `service install` on a legacy installation | Apply the TOML and record the completed installation. |
| `service reinstall` | Remove the service registration, old scripts, installed TOML, and provisioning records; install again from the bundled TOML. Preserve managed data. |
| `service uninstall --purge` | Remove the service registration and all managed files, including legacy scripts, TOML, disks, keys, and downloads. Windows also unregisters `firecrab-debian`. |

Plain `service uninstall` continues to preserve managed data for a later install.

## Explicit installation and migration

Run either command with a CLI that embeds the new manifest:

```sh
firecrab service install
firecrab service reinstall
```

Both commands select the bundled TOML, validate it, and verify its artifact
checksums. The guest reports the manifest fingerprint after provisioning and
the nested Firecracker check pass. The CLI saves a validated TOML copy at
`<managed-home>/micromanager.toml` after the resident service and local API become
healthy. Failed provisioning does not publish a new installed manifest.

The installed copy records the selected release inputs. It is used for later
download validation and offline OS repair. It is not a user configuration
override: the next explicit installation selects the new CLI's bundled manifest.

### macOS

The first migration replaces the replaceable Debian OS disk and its boot files.
It preserves the persistent data disk and management SSH client keys. It clears
the cached SSH server fingerprint because the new OS generates new server keys.
A different manifest also reprovisions the OS. Repeating an install with the same completed
manifest preserves it.
`reinstall` always rebuilds the OS and boot files, even when the TOML is unchanged.

Provisioning installs the exact Debian kernel named in the TOML. It extracts that
kernel into `system/Image` and copies its matching initrd into `system/initrd.img`.
It does not select the older kernel running during EFI provisioning. The nested
check uses the selected kernel, and installation verifies that the resident
management VM boots it before publishing the installed manifest.

### Windows

Migration reprovisions the existing `firecrab-debian` WSL distribution. It does
not unregister or reimport it, so its data remains in place. The rootfs image is
used only when importing a missing distribution. A completed matching manifest
is reused by `install`; `reinstall` runs the guest provisioner again.
Reinstallation removes the old task registration, installation scripts, and
completion records before rebuilding them. TOML provisioning also removes
Firecrab's development service overrides so the release installer starts its
installed binaries. User service overrides and distribution data remain in place.

The TOML's Debian kernel is installed and used for the nested Firecracker check.
It is separate from Microsoft's shared WSL kernel. Migration does not replace
the WSL kernel or change the user's other WSL distributions. `service doctor`
continues to check WSL and nested KVM capabilities.

## Changing release inputs

Update the versions, download URLs, and upstream checksums together. Update each
architecture's Debian kernel version to an available Debian 13 package.
Unsupported TOML schemas, unknown fields, unsafe filenames, and malformed
checksums fail before downloading or stopping the service.

An unavailable pinned kernel package fails provisioning. It does not fall back
to a different kernel. Publish an updated manifest when Debian's package mirrors
no longer provide the selected kernel.
