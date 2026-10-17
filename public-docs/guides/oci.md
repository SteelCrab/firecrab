# OCI images

Import a container image as a bootable VM template.
Firecrab merges verified layers, adds guest services, creates ext4, and pairs a host-compatible kernel.
No catalog image needs to be installed first.

## Inspect

```sh
firecrab image inspect nginx:1.27
```

Inspection returns compatibility, manifest digest, and the alias an import would claim.
It downloads no config or layers.
References use Docker pull syntax; a bare name uses Docker Hub `library` and `latest`.
HTTPS is required except for `localhost` and `127.0.0.1` registries.
A missing host architecture is rejected.

REST equivalent: `GET /api/oci/inspect?reference=nginx:1.27`.
Registry metadata inspection allows 60 seconds for authentication and bounded connection retries; the CLI waits 65 seconds.
HTTP errors and partial blob downloads are not retried.

## Import

```sh
firecrab image import nginx:1.27
firecrab image import-status nginx-1.27
```

Import returns when the API accepts the background job.
Poll the alias until `succeeded` or `failed`; a failed CLI snapshot exits `1` with its last nonempty log line.
Success makes the alias available on **Images** and in VM creation.
`nginx:1.27` becomes `nginx-1.27`.

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/oci/import \
  -H 'Content-Type: application/json' \
  -d '{"reference":"nginx:1.27"}'
curl -fsS http://127.0.0.1:5523/api/oci/import/nginx-1.27
```

| Failure | Response |
| --- | --- |
| Invalid reference | `400 validation_failed` |
| Catalog or installed alias collision | `409 alias_collision` |
| Active job for that alias | `409 import_in_progress` |
| Exhausted Docker Hub anonymous quota | `429` |

`fakeroot` must be installed on the API host for ext4 packaging.
Limits default to 16 GiB per compressed blob, 64 GiB per decoded layer, and 32 GiB per rootfs.
See [Operations](operations.md#api-configuration) to change them.

## Boot and connect

Create a VM with the installed alias and a MicroNetwork.
The imported image preserves supported systemd/OpenRC init; other images use BusyBox.
Its container entrypoint runs as a service after network readiness.
`EXPOSE` metadata does not create host port forwards; configure them on the VM.

First boot may install SSH and other packages.
Network readiness does not establish SSH readiness.
Activation scripts are embedded at import time; host upgrades do not update existing templates.
Back up an unused template before replacing it with a fresh import.

## Registry login

Save a Docker Hub personal access token through **MicroRegistry** or the [login API](../reference/api.md#docker-hub-login).
The host uses it for the requested image and toolbox pulls.
The secret is write-only; other registries remain anonymous.
For socket permission errors, inspect the [host diagnostics](troubleshooting.md#oci-pull-fails).

## Related

- [Import pipeline and safety rules](../reference/oci-import.md)
- [Guest runtime](../reference/guest-runtime.md)
- [Images](images.md) and [Kernels](kernels.md)
- [Dashboard SSH](dashboard.md#console-and-ssh)
