# Automatic replica recovery

The API probes running host-local MicroServices every five seconds.
Three consecutive failures mark a replica unhealthy. Two successful probes restore readiness.
The scan can take longer when several probes time out.

Recovery uses the shared Deploy permit. An unhealthy replica is removed from Nginx,
then restarted. If restart cannot restore readiness, a new VM replaces that replica.
Healthy replicas keep their VM IDs. The service keeps its release ID, image digest,
captured encrypted Secret values, network, ports, and configuration.
Applications must keep persistent data in external storage; replacement creates a fresh disk.

If every replica is unhealthy, the owned hostname returns HTTP 503.
Named and Quick Tunnel routing remains configured. Only ready endpoints rejoin Nginx.
Old Nginx workers must finish before a failed VM is stopped or deleted.
A drain timeout retains the VM and its recovery journal.

The deployment response includes `recovery.replicas` and `recovery.repair`.
They show probe counts, retry times, attempt counts, VM IDs, and the next repair step.
Intent and candidate VM IDs are saved before lifecycle side effects.
API restart resumes the saved step without creating another candidate.
No Secret values appear in this status or in release history.

Retries wait 5, 10, 20, then 40 seconds after failed attempts.
At most five repairs are allowed until a replica stays healthy for 60 seconds.
Exhausting the budget pauses recovery and keeps unhealthy endpoints out of Nginx.
Fix the cause and use Deploy, or Stop and deploy again, after recovery pauses.
Stop removes all recorded source/candidate VMs and prevents automatic resurrection.
As with other Deploy work, an active repair must finish or pause before Stop is accepted.

Git-managed services retain their pinned inputs and configuration ownership during maintenance.
The host loop skips controller-owned worker services; cross-host recovery is separate work.
Records without a valid pinned digest or network require an explicit Deploy.

## Verification

```sh
cargo test -p firecrab-api service_runtime::recovery --locked
cargo test -p firecrab-helper-protocol service_routes --locked
sudo python3 scripts/ci-qa-deploy.py --disposable-host --replica-recovery --report target/qa/replica-recovery.json
```

Run the last command only on an empty disposable Linux/KVM host.
It checks VM exit/restart, unhealthy replacement, held requests across API restart,
all-unready HTTP 503, captured values, unrelated VMs, Stop, and cleanup.
