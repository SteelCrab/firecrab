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
scripts/ci-qa-macos-e2e.sh repair
```

The wrapper prefers this checkout's `target/debug/firecrab`.
The installed helper, CLI, disks, and SSH key remain in place.
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

## Results and restoration

Results default to `target/qa/macos-repair/<timestamp>/`:

```text
run.log                exact commands, output, and exit codes
summary.json           platform, source revision/dirty state, checks, and warnings
registration-backup/   original launchd plist and daemon wrapper
```

The result directory is private to the current user.
Logs contain hashes, not SSH private keys or guest unit contents.
The test resumes a paused tunnel and restores a modified ready marker in `finally`.
After a later failure, it restores the original registration and restarts the
service. A restoration failure is recorded separately and never hides the test
failure. Backups remain available for inspection.

The Python guard/restoration contract runs in GitHub CI without virtualization:

```sh
python3 -m unittest scripts/test_macos_repair_qa.py
cargo test -p firecrab-cli --test macos_service_recovery --locked
```

The Rust command checks native CLI messages and JSON on macOS.
The runtime phase needs the real installed management VM and runs separately.
See [TEST](TEST.md), [macOS microManager](micromanager-macos.md), and [CI](ci.md).
