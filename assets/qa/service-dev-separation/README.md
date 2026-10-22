# Development service control evidence

Captured in **Ghostty on macOS, 2026-10-22 (KST)**.

These are isolated regression tests of the embedded guest script. They reuse
`scripts/test-micromanager-dev.py` with temporary source, cache, binary, and unit
directories. `systemctl`, `curl`, and compiler commands are stubbed. The screenshots
show actual test execution in Ghostty, rather than live management VM operations.
The installed management VM and its development dashboard were left unchanged.

| Action | Result | Screenshot | Original log |
| --- | --- | --- | --- |
| `start` | PASS | [start.png](start.png) | [start.log](start.log) |
| `stop` | PASS | [stop.png](stop.png) | [stop.log](stop.log) |
| `restart` | PASS | [restart.png](restart.png) | [restart.log](restart.log) |
| `clean` | PASS | [clean.png](clean.png) | [clean.log](clean.log) |

Run from the repository root:

```sh
python3 scripts/qa-service-dev-controls.py start
python3 scripts/qa-service-dev-controls.py stop
python3 scripts/qa-service-dev-controls.py restart
python3 scripts/qa-service-dev-controls.py clean
```

Each action checks service operation order and absence of compilation. All actions
preserve fixture user data, custom overrides, and the host source checkout.
`start`, `stop`, and `restart` preserve deployed development binaries and caches.
`clean` removes development overrides, binaries, sources, toolchain/build caches,
and hidden cache files after mocked release readiness succeeds. It preserves the
deployment lock inode.

The full contract suite also covers legacy helper names, lock contention,
read-only filesystems, failed restoration, and rollback. Rust tests cover CLI
parsing, backend dispatch, and owned dashboard cleanup. Live macOS/Windows runtime
acceptance remains separate from this evidence.
