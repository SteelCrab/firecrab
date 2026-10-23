# Terraform REST API proof

This experimental example uses the published [Mastercard/restapi 3.0.0 provider](https://registry.terraform.io/providers/Mastercard/restapi/3.0.0/docs).
It calls Firecrab's public REST API directly, without CLI wrappers or database access.
It manages one MicroNetwork, one stopped MicroVM and one saved MicroService configuration.
It is separate from the native YAML stack planner in #437.
It does not implement stack apply, continuous Git reconciliation or asynchronous deployment lifecycle management.

## Prerequisites

- Terraform 1.10 or later and a trusted Firecrab API connection.
- An installed whoami template serving HTTP on port 80, requiring at most 1 GiB of disk.
- An unused resource prefix, subnet and host TCP port.
- A real digest-pinned service image. Saving the configuration does not pull or deploy it.

Run in a disposable directory so state stays outside the source checkout:

```sh
run_dir=$(mktemp -d /tmp/firecrab-terraform.XXXXXX)
cp examples/terraform/basic/*.tf "$run_dir/"
cp examples/terraform/basic/.terraform.lock.hcl "$run_dir/"
cd "$run_dir"
```

Create `demo.auto.tfvars` using values for the intended host:

```hcl
api_url       = "http://127.0.0.1:5523"
name          = "unique-demo"
subnet_cidr   = "172.30.237.0/24"
template      = "traefik-whoami-latest"
host_port     = 18387
service_image = "docker.io/traefik/whoami@sha256:c4717a8d1f0134a7444e24f881160e033991f23027c6c5a9a3f8fd22e70d1d44"
```

Inspect existing networks and ports before choosing these values.
The template alias must already exist on that host.
Verify the image digest independently for the intended application.

```sh
terraform init
terraform validate
terraform plan -out=create.tfplan
terraform apply create.tfplan
terraform plan -detailed-exitcode
terraform output -json
```

The second plan must report no changes and exit `0`.
The network's detail endpoint nests its CIDR, so the example refreshes it by UUID from the collection endpoint.
Known response fields omitted from `data` are listed in `ignore_changes_to`.
Do not enable `ignore_server_additions` with restapi 3.0.0: live testing showed it preserves stale desired data on refresh and misses declared-field drift.
New API response fields can require updating the ignore lists.
Network name/CIDR changes and all VM configuration changes replace those resources.
Service edits use the existing full-replacement `PUT` endpoint and require an undeployed service.

## Runtime checks and cleanup

Terraform creates the VM in `created` state.
Start it with the public `POST /api/vms/{id}/start` endpoint and poll `GET /api/vms/{id}` until `running`.
Check HTTP through the host's forwarded port.
On macOS, that port belongs to the management VM; use its reachable address or a loopback SSH tunnel.
Service deployment and its health wait are also separate public API operations.
Neither runtime action is represented by this example's Terraform state.

Before destroying, stop only this example's VM with `POST /api/vms/{id}/stop`.
If the service was deployed, stop it and wait for cleanup before deleting its configuration.
Do not delete existing templates or resources belonging to other workloads.

```sh
terraform plan -destroy -out=destroy.tfplan
terraform apply destroy.tfplan
terraform state list
```

Confirm the exact output UUIDs return `404` and the state list is empty.
Preserve the state and logs until cleanup succeeds.
State and plan files can contain API responses and plaintext environment values; use only test values here.
