# MicroService domain live QA

Run this test on a dedicated Linux/KVM host with an empty Firecrab API and Pipe
configuration. The API and privileged helper must use the same private installation
paths. Install the bundled ingress engine, the pinned cloudflared engine, and the
normal OCI import prerequisites. The test uses the local OCI fixture; it does not
pull an application image from Docker Hub.

```sh
sudo python3 scripts/test-microservice-domain-live.py \
  --api http://127.0.0.1:15335 \
  --restart-unit firecrab-qa335-api.service \
  --output /opt/firecrab-qa335/live-report.json
```

The runner accepts only a loopback API and a `firecrab-qa335-` restart unit. It
requires an empty API and absent Pipe configuration before creating resources.
It starts ingress on `127.0.0.1:18335`, obtains a temporary Quick Tunnel URL, and
uses that hostname as the MicroService domain. The HTTP fixture exposes a health
marker and its VM MAC address. HTTPS certificate verification stays enabled.

The runner deletes its connector before stopping the service, then removes its
VMs, services, routes and MicroNetworks. Its `finally` block also attempts cleanup
on failure. The operator must stop the dedicated API/helper and ingress units
and remove the disposable installation after saving the report. Inspect any
`cleanupErrors` before removing the installation.

For an independent external client check, add `--external-ack PATH`. After the
report reaches `awaiting_external_probe`, request its `publicUrl` from another
machine with certificate verification enabled. Check `/cgi-bin/identity` against
the report's marker and hostname. Write a JSON acknowledgment to `PATH` with the
matching `marker` and `publicUrl`, plus the observed HTTP status and TLS evidence.
The runner waits up to 180 seconds for that acknowledgment.

## Measured result: 2026-10-23 KST

Source: `SteelCrab/firecrab`, branch `feat/335-microservice-domain`, uncommitted
feature snapshot after base commit `194a26daa6af9869215896d223399ba563a6a14b`.
The source file hashes and installed binary hashes were recorded with the logs.
This is a source QA result, not a released-build or named-tunnel DNS claim.

Host: MSI Linux x86_64, Ubuntu 26.04.1 LTS, native KVM, Firecracker 1.17.0.
The API, helper and CLI used native debug builds. Ingress used the bundled static
Nginx 1.30.6 engine. The connector used pinned cloudflared 2026.10.0.

| Check | Result |
| --- | --- |
| Deploy two real MicroVM replicas using `{}` | PASS; automatic network, healthy replicas, no host forwards |
| Public HTTPS health request | PASS; trusted certificate, TLS 1.3, HTTP 200 |
| Twenty public identity requests | PASS; both VM MAC addresses observed; original Host and `https` scheme preserved |
| Stop and restart the dedicated API | PASS; five HTTPS requests succeed while API is stopped; ingress, connector and both VM PIDs stay unchanged; service is re-adopted |
| Deploy an app with a failing health path | PASS; failed VM removed, no owned domain route, healthy public service unaffected |
| Stop one serving MicroVM | PASS; eight public requests reach the surviving VM |
| Independent macOS client | PASS; system curl verifies the certificate (`ssl_verify_result: 0`), HTTP 200, expected marker, Host and HTTPS scheme |
| Resource cleanup | PASS; zero VMs, services, MicroNetworks, routes and Quick Tunnels; QA API/helper/ingress stopped; QA unit files and installation removed |

The temporary public URL was retired after the checks. No long-lived public
endpoint or named-tunnel DNS record was created. Local evidence files are
`live-report.json`, `live.log`, `runtime.log`, `source-manifest.json`,
`environment.txt` and `cleanup.log` in the retained validation output directory.
