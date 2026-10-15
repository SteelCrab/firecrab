# macOS microManager repair QA

This runtime E2E checks G7 repair and G7a recovery advice on an installed Mac.
It uses the existing management VM and restarts it several times.
It refuses running/starting/stopping MicroVMs, unknown VM states, symlinked
managed paths, and a launchd job belonging to another managed home.
Stop other imports or builds before the run.

## Build and run

Use a native Mac whose signed helper and guest have completed installation.
The local API must answer and the managed service must be loaded.
The test does not install, reinstall, download, or deploy source binaries.

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
scripts/ci-qa-macos-e2e.sh repair
```

The wrapper prefers this checkout's `target/debug/firecrab`.
The installed helper, CLI, data disk, and SSH key remain in place.
The healthy runtime scenario also requires the original OS disk identity.
Default managed paths are the ones used by `service install`.
For a custom installation, set both variables to its existing absolute paths:

```sh
export FIRECRAB_INSTALL_DIR=/absolute/path/bin
export FIRECRAB_MICROMANAGER_HOME=/absolute/path/micromanager
python3 scripts/ci-qa-macos-repair.py \
  --cli ./target/debug/firecrab \
  --results-dir ./target/qa/repair-manual
```

The results directory must not already exist.

## Checks

1. G7: verify an idle service, matching launchd registration, and API catalogs.
2. G7: stop the service, run repair, and require live SSH/guest services and HTTP 200.
3. G7: remove the plist while the job remains loaded. Require `start` to suggest
   repair without changing the running VM. Repair must recreate the plist.
4. G7: stop the service and remove its wrapper. Require the same advice and recovery.
5. G7a: temporarily set the ready marker to a test-only unreachable SSH address.
   `dev --restore` must fail before deployment and suggest repair. Restore the
   original marker, run repair, and confirm the API and guest services.
6. G7a: pause the verified managed SSH tunnel. `status` must report failure and
   suggest repair. `debug --json` must remain valid JSON with recovery advice.
   Resume the tunnel, run repair, and confirm recovery.

Each recovery checks installed CLI/helper and boot-asset SHA-256 values, OS/data
disk file identities, artifact inventory, pinned SSH host keys, and API catalog
identities. Disk contents change during boot; the disk files must stay the same.
The test compares a hash of the guest systemd definitions to preserve development
overrides without recording their contents.
If initial SSH is already unavailable, the first repair also checks recovery
from that condition. Results record a WARNING because the guest configuration
baseline can only be captured after that repair.

## Progress (G7b)

Before each operation, require `[REPAIR 1/6]` through `[REPAIR 6/6]` for
validation, shutdown/helper cleanup, OS backup/repair, registration, VM/API
startup, and guest-boot/API checks.
Pending readiness and shutdown waits must print elapsed time and current state
again every ten seconds. The local VM/API readiness deadline is ten seconds;
shutdown keeps its forty-second deadline. An unready VM/API must time out instead
of reporting success. A failed validation must not print the shutdown stage.
Each successful runtime repair also checks G7b for all six stages in order.
An unhealthy guest must still fail; progress output does not mean recovery passed.
Use `service debug --logs` to inspect the console when boot cannot finish.
The native daemon tests exercise pending-to-ready, repeated progress through a
timeout, and immediate lost-service/probe-error returns without booting a VM:

```sh
cargo test -p firecrab-cli --bin firecrab micromanager::macos::daemon::tests --locked
```

## SSH paths and boot report (G7f)

The default `Application Support` path must remain one OpenSSH known-hosts path.
Native tests use the real `ssh -G` parser, including spaces, quotes, backslashes,
and percent tokens. The executable wrapper reconnect test checks this parsing
as well. Shell, development upload, diagnostics, and port forwards share it.

OS recovery uses the serial console. Repair's final check reads the fresh guest
boot report through virtiofs and verifies the live API. It does not run an SSH
command. Invalid schema, mismatched IP, inactive, missing, or duplicate service
fields must fail. Existing SSH host pins must remain intact.

## Offline OS recovery (G7c/G7d/G7e)

The helper uses `break=premount` from [Debian initramfs-tools](https://manpages.debian.org/trixie/initramfs-tools-core/initramfs-tools.7.en.html).
It checks only `/dev/vda1` on a private OS copy. A failed read-only check may run
`fsck.ext4 -f -y`; a second read-only pass must return zero before commit.
[Debian e2fsck](https://manpages.debian.org/trixie/e2fsprogs/e2fsck.8.en.html)
documents the accepted correction exits (0–3) and unresolved errors (4).
The VM attaches no data disk, directory share, or network. A verified cached
Debian reference may be attached read-only for missing packaged OS files. VM/API readiness
keeps its ten-second deadline; offline filesystem checks use 120 seconds.

Create tiny normal and unattached-inode fixtures under temporary storage.
The generator requires existing e2fsprogs tools; it reads only the chosen kernel
and initrd. It refuses an existing output directory.

```sh
python3 scripts/create-macos-repair-fixture.py \
  --kernel /absolute/path/system/Image \
  --initrd /absolute/path/system/initrd.img \
  --output /private/tmp/firecrab-repair-fixtures
python3 scripts/ci-qa-macos-os-repair.py \
  --helper "$PWD/target/debug/firecrab-micromanager-macos" \
  --fixture-home /private/tmp/firecrab-repair-fixtures/corrupt \
  --results-dir "$PWD/target/qa/os-repair-corrupt"
```

Repeat with `clean` and a new results directory. Require helper exit zero,
`verify_exit=0`, and original OS/data/SSH hash and inode preservation. The corrupt
case must report `changed=true`; the clean case skips correction. Logs and JSON
snapshots remain after failures. Installed/default homes, symlinked paths,
aliased staged images, and open images are rejected before helper execution.
Native Rust tests verify commit, backup preservation, timeout rollback, and
scoped cleanup of disposable helper processes. Swift tests check VM locking,
recovery-copy validation, and stop completion without a delegate callback.

For G7e, use a marked disposable Debian fixture with a missing `libdbus` link
and generator directory. Place the authenticated cached image beside its staged
copy as `debian-system.reference.raw`. The runner includes it in preservation
snapshots and passes `--reference` to the helper. Require `restored_files > 0`,
`changed=true`, and `verify_exit=0`, even when the initial filesystem is clean.
Repeat with `--expect-restored-files 0` to check that another repair restores
nothing. Version/checksum mismatches must fail without committing an OS image.
Boot only repaired disk copies in a separate managed home and check live SSH,
`dbus`, network services, and the guest API within the ten-second readiness limit.

## Results and restoration

Results default to `target/qa/macos-repair/<timestamp>/`:

```text
run.log                exact commands, output, and exit codes
summary.json           platform, source revision/dirty state, checks, and warnings
preservation-*.json    before/after hashes and file identities, including failed runs
registration-backup/   original launchd plist and daemon wrapper
```

The result directory is private to the current user.
Logs contain hashes, not SSH private keys or guest unit contents.
The test resumes a paused tunnel and restores a modified ready marker in `finally`.
After a later failure, it restores the original registration and restarts the
service. A restoration failure is recorded separately and never hides the test
failure. If the restored service cannot start, the test stops it to prevent
repeated failed boots and records cleanup separately. The final file-preservation
audit runs even when recovery fails.
Backups remain available for inspection.

The Python guard/restoration contract runs in GitHub CI without virtualization:

```sh
python3 -m unittest scripts/test_macos_repair_qa.py scripts/test_macos_os_repair_qa.py scripts/test_macos_os_files.py
cargo test -p firecrab-cli --test macos_service_recovery --locked
```

The Rust command checks native CLI messages and JSON on macOS.
The runtime phase needs the real installed management VM and runs separately.
See [TEST](TEST.md), [macOS microManager](micromanager-macos.md), and [CI](ci.md).
