# Project workspace recovery: 2026-10-30

## Result

PASS for 12 backend checks on Ubuntu 26.04/aarch64 with real KVM. The harness
exited with status zero and its final installed-service preservation check passed.
This record does not complete issue #550 or authorize merging it. PR #553 remains
reverted from main; implementation stays on `feat/550-ai-canvas` until the whole
issue is implemented and accepted.

## Environment and provenance

- UTC: `2026-10-30T06:30:31.107656+00:00` to `2026-10-30T06:34:03.083726+00:00`.
- Isolated, root-owned API/helper in a private network namespace inside the
  macOS managed Linux guest, with actual `/dev/kvm` and systemd workload units.
- Firecracker v1.17.0; Ubuntu `ubuntu-26.04-v5`; 1 vCPU and 512 MiB workload RAM.
- API/helper from [CI run 41001127095](https://github.com/SteelCrab/firecrab/actions/runs/41001127095),
  artifact `firecrab-host-aarch64-gnu`, source `e514ce291221b58f8bb1490cd50ef80422785267`.
- API/helper source matches the staged foundation commit `fce090d`.
- The installed API remained PID `572`. No installed service was restarted.
  The workload-unit inventory was empty before and after the run.

| Evidence | SHA-256 |
| --- | --- |
| API | `e1078d5bf9c1028c69007d0af93781150830c7a7c695e16c611ebcfff7f5be6e` |
| Helper | `60560318af8c37fad0152ec90daec6fcbebe3663dca436192bc8af1c6c3731d8` |
| Executed harness | `af825f123404533407d266591ba4ca980d4e4da2cb5230b07a97bb8cd3d19b2d` |
| Ubuntu kernel | `d171d906729b5548b7400f12de09cb3dd08a06a6e2b74075c8b9e9482e720aa1` |
| Ubuntu rootfs | `c5700031e35f10bcb68e73c28b1f3740602bffc4f55417a27034681982f5247a` |
| Private summary | `f67ef962273d61a0a02d9b928f4e890b685a44591d361870b0a384d955ace168` |
| Private evidence archive | `e4802849ce71c2582bd7b988115d780b69025429e37e415e9cd9080a87c4b2c0` |

## Verified behavior

- A real ext4 mount accepted a synced file, with unchanged project metadata.
- A disconnected HTTP client could be replaced without losing project state.
- Graceful API restart and SIGKILL recovery retained the running workload PID,
  guest boot, database identity, volume identity, document revision and file hash.
- Project metadata remained queryable with the workload VM stopped.
- VM stop/start changed the guest boot and required fresh mount reconciliation.
- Deleting the old stopped VM retained the project database and data volume.
- A stale generation could not change the runtime binding.
- A new VM used the same database and volume at generation 2. The file hash matched.
- Repeating an identical binding kept generation 2.
- A temporarily missing retained disk reported unavailable without creating a disk.
- Ordinary project deletion, volume deletion and direct detach returned conflicts.
- Every observed workload unit ran inside the owned network namespace.

Project: `531f899d-054c-4eff-8f36-880d3d30a310`. Database: `db112dac-c1e5-4cc2-8613-07084ada798a`.
Volume: `7d9872de-7fd8-4402-af20-f83f8fcb294e`.
Retained file SHA-256: `3b7a5c6ee606e91c44a8f5b5552a5f22667fecbfb40609bc6ba803caa1f8cd85`.

## Remaining qualification

- HTTP client reconnection verifies backend state. Browser tabs, canvas/editor
  restoration, model sessions and terminal processes need their later integrations.
- The installed daemon, its service-user sandbox and multi-user access were not
  exercised by this root-owned fixture.
- Alpine `alpine-3.24.1-v5` booted and acquired DHCP after the fixture permission
  fix, but workspace reconciliation returned HTTP 409. Alpine remains unqualified.
- Earlier fixture permission errors and a harness expectation that incorrectly
  rejected identical binding retries were corrected. Failed reports were retained.
- Writes were explicitly synced. No unflushed power-loss guarantee was tested.

Use the [recovery gate runbook](project-workspace-recovery-qa.md) to reproduce the
checks. Its eight ownership/process guards also passed on Linux. The summary and
logs are retained privately; VM keys and full database/disk copies are not published.
