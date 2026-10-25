# Fifteen-service Deploy acceptance

Run this acceptance only on an empty, disposable Linux host with working KVM.
It installs local OCI fixtures and uses the real API, helper, Firecracker and
bundled Nginx. It does not verify external registry credentials or public TLS.

Requirements:

- At least 16 available CPUs: 15 one-CPU services and one rolling surge replica.
- At least 6,400 MiB available RAM, including VMM overhead and host reserve.
- At least 16 GiB free on the installed image/replica filesystem.
- A source installation with the API/helper services running and `busybox-static`.
- No existing VMs, MicroServices, pools, Auth credentials or Pipe configuration.

The standard four-CPU GitHub runner cannot run this acceptance. Do not override
CPU accounting or distribute the services across hosts to obtain a pass.

```sh
sudo python3 scripts/ci-qa-deploy.py --disposable-host \
  --fifteen-services --report target/qa/deploy-fleet.json
```

The runner first checks update, drain retry, selected rollback and partial-failure
recovery with one/two replicas. It then starts 15 separate services at once. It
keeps HTTP traffic on every domain during queued updates, restarts the API during
an active drain with pending Deploys, rejects an excessive CPU/surge request and
rolls every service back to its original image and environment configuration.

The JSON report records the source commit, installed binary hashes, host capacity,
queue observations, peak live VM/vCPU counts and per-domain HTTP request counts.
Passing requires zero HTTP errors, a maximum of one active Deploy and exactly one
observed surge above the 15 running replicas. The runner removes its services,
VMs, network, routes, image fixtures and test credentials before reporting success.
Failed acceptance writes a failed report and attempts the same cleanup.

Keep issue #335's 15-service item unchecked until this live acceptance passes.
