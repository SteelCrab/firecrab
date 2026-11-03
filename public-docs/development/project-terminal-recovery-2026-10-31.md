# Authenticated project PTY recovery: 2026-10-31

Related: #550, Draft PR #562. This report covers real project PTYs, independent
mutual-TLS callers, retained output and runtime recovery. Editor, agent,
skill-store and cross-host restore acceptance remain separate work.

## Source and environment

- Source: `69ccfd4359022e0d3835bee644751659878f48bc`.
- Run UUID: `464d36be-7137-45eb-979f-64699b0473af`.
- UTC: `2026-10-31T12:33:17.778093` to `2026-10-31T12:37:00.577384`.
- Linux aarch64, Firecracker 1.17.0, real nested KVM, Ubuntu 26.04 v5.
- API SHA-256: `48b7d2e9e5b9ff24a3feda426c72eace00b307d3340b4a3f757e47a0270119af`.
- Helper SHA-256: `4cce02d3618f82b2ec4021672f5bcc767d00d29732c82114422f407b6f00944f`.
- Static guest helper SHA-256: `696fe9abc84701770b877d47d0c70a52cb9e41774a3ba67f820a6ecf2ca078d0`.
- Main harness SHA-256: `e78bdd83a46da4a1184654a480b875ba0f30f3116339de1506ed7bcf6a61e89c`.
- PTY harness SHA-256: `3504d87b76e23c8d745df95ac46a59665d2a025bc2d1ff54e7524db07687df78`.

The fixture used its own CA, four distinct certificate identities, HTTPS API,
helper, network namespace, control database, project database and retained ext4
volume. Certificate grants were enforced by the actual API. No identity headers,
mock guest, system trust-store changes or shared production storage were used.
API/helper fixture processes ran as root. Production service-user acceptance is
still pending. PTY shells ran under separate unprivileged UIDs with a common
project group and no controller/provider credentials.

## Results

**26/26 gates passed.** The file/recovery gates from the earlier report passed
again. The additional PTY and authentication observations were:

| Scenario | Observation |
| --- | --- |
| Two authorized editors | Both read one live PTY; only the current controller could send input. |
| Viewer | Read output; input, control claim, file writes and unrelated projects were rejected. |
| Control takeover | A second editor took control at the expected generation; the old token was rejected. |
| Resize | `stty` and another client's retained metadata both reported 42 rows and 111 columns. |
| Multiple terminals | Different process UIDs and environments; retrying creation retained the same shell. |
| API restart and SIGKILL | Reattached to the same supervisor/child PID, start ticks and kernel boot; a one-shot command remained executed once. |
| Restarted controller lease | The old API-owner lease was rejected; the caller explicitly claimed a new lease. |
| Viewer grant revocation | An actual TLS WebSocket closed and later reads returned 403. |
| Creator grant revocation | Only that creator's terminal stopped; the other creator's original shell continued. |
| Stopped workload VM | Project records, file history and retained PTY output remained queryable over mutual TLS. |
| VM stop/start | The kernel boot changed; the old PTY became interrupted, retained history and did not replay commands. |
| Replacement VM | The retained volume, file history and old PTY output remained available; the old shell was never relaunched. |

The original primary shell had UID `572962019`, supervisor PID `461`, child PID
`463`, supervisor start ticks `4399` and child start ticks `4405`. Those process
identities remained unchanged across both API restarts. The kernel boot changed
from `9c668d2b-dc6c-410a-a76c-215dfcf9efe7` to
`f77e2633-4849-4016-9071-ba46829aac1a` on VM reboot, then to
`1128d3ff-5d2b-4c63-b2ba-ab71eab4d075` on runtime replacement.

The ownership gate confirmed the installed API PID and pre-existing workload
units were unchanged. Owned fixture metadata and retained data were preserved.
These checks cover synced writes. They do not establish unflushed power-loss,
browser UI, real editor or real model acceptance.

## Reproduction

Prepare a fresh, separately owned Linux/KVM root as described in
`scripts/ci-qa-project-workspace.py`. The completed invocation was:

```sh
python3 /opt/q550464d36be/qa.py --root /opt/q550464d36be --run-id 464d36be-7137-45eb-979f-64699b0473af --template ubuntu-26.04 --files --tls --terminals --live
```

That UUID identifies the completed retained fixture. Use a new run UUID and root
for another execution. Do not rerun against its retained data.
