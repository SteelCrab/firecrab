# Stack validation

Validate a declarative YAML stack before committing it to Git.
This is the first part of [GitOps IaC #437](https://github.com/SteelCrab/firecrab/issues/437).
The experimental `firecrab.dev/v1alpha1` schema may change.

## Run

```sh
firecrab stack validate --file examples/stacks/basic.yaml
firecrab stack validate --file examples/stacks/basic.yaml --json
```

The [example](../../examples/stacks/basic.yaml) contains one network, one standalone VM, and one service.
Its service digest is a placeholder, not a published image.
The installed template alias must also be replaced for a future deployment.

Validation reads only the selected UTF-8 file.
It does not read host profiles, start microManager, contact an API or registry, or change resources.
Global `--api` and `--host` flags have no effect on this command.
Exit `0` means validation passed; input and validation failures exit `1`.
CLI usage errors, such as omitting `--file`, exit `2`.

The JSON report is written to stdout for both success and failure.
Successful output contains only the stack name and resource counts:

```json
{
  "valid": true,
  "stack": "demo",
  "resources": {
    "microNetworks": 1,
    "microVMs": 1,
    "microServices": 1
  },
  "errors": []
}
```

Failures have `valid: false` and an `errors` array with `path` and `message`.
YAML parsing errors also include `line` and `column` when available.
Messages omit submitted values and source excerpts.

## Schema

```yaml
apiVersion: firecrab.dev/v1alpha1
kind: Stack
name: demo
microNetworks:
  app:
    name: demo-app
    subnetCidr: 172.31.20.0/24
microVMs:
  worker:
    name: demo-worker
    template: alpine-3.24.1
    network: app
    cpu: 1
    ram: 256
    diskGb: 2
```

The stack name and map keys are lowercase DNS labels of 1-63 bytes.
They start and end with a letter or digit and may contain hyphens.
Map keys are logical identities, separate from each resource's API `name`.
Changing a file path or YAML map order does not change those identities.
Network and VM API names allow 1-64 ASCII letters, digits, dots, underscores, and hyphens.
Names must be unique within each resource group.
No resource is adopted from the host by name.

| Group | Required fields | Optional fields |
| --- | --- | --- |
| `microNetworks` | `name`, `subnetCidr` | `internetEnabled` (default `true`) |
| `microVMs` | `name`, `template`, `network`, `cpu`, `ram`, `diskGb` | `templatePin`, `egressPolicy` (default `internet`), `storageRoot`, `env` |
| `microServices` | `config`, `network`, `hostPort` | Defaults inside `config` follow the MicroService API |

All three maps may be omitted, but the stack must contain 1-256 resources in total.
Every `network` must match a key in `microNetworks`.
External network UUIDs and cross-stack references are not accepted yet.
Only one YAML document is allowed, up to 1 MiB including comments.
Duplicate keys, unknown fields, unsupported versions, and wrong field types fail validation.
Boolean fields require `true` or `false`.
There is no environment interpolation or file inclusion.
YAML anchors, aliases, and merge keys are rejected.

Domain services use `hostPort: 0` and an Nginx-backed `config.domain`.
They can declare [automatic registry Deploy](deploy.md#automatic-registry-deploy)
under `config.autoDeploy`. Keep `config.image` digest-pinned for initial deployment.
A later apply preserves the running automatic image and reconciles other settings.

## Local constraints

- IPv4 prefixes must be /16 through /28; declared ranges must not overlap.
- CPU must be 1-32; RAM must be a power of two between 128 and 32768 MiB.
- VM disks must be 1-500 GiB; the image-specific minimum needs host validation.
- Template aliases and storage IDs use 1-64 safe ASCII name characters.
- `env` allows 64 POSIX names, 256 bytes per name, and 4096 bytes per value without NUL.
- Service configuration reuses the [MicroService API](microservices.md#fields) fields and defaults.
- Service images must have an `@sha256:` suffix with 64 hexadecimal digits.
- Services allow 1-100 replicas; each consecutive host TCP port must fit in 1-65535.
- Host port ranges must not overlap between declared services.
- Domain services require zero host port. Secret references use existing PipeSecrets IDs; inline Secret values are not supported.

Keep credentials out of `env` and Git.
Use `env` only for non-secret settings.
The command does not print configuration values, including on failure.

## CI

Run the same command after building the CLI:

```sh
cargo run -p firecrab-cli --locked -- stack validate --file examples/stacks/basic.yaml --json
```

The repository's Rust CI job validates the example with this command.
The shared `firecrab-stack` crate provides the parser and local validation for later API and controller use.

## Limits and next steps

Passing offline validation does not establish deployment readiness.
Offline validation does not check host image availability, image-specific disk minimums, registered storage, capacity, or existing resource conflicts.
`templatePin` declares `version`, `kernelSha256`, `rootfsSha256`, `initrdSha256` (nullable), and `bootArgsSha256`.
Hashes must contain 64 lowercase hexadecimal characters. The host plan resolves and checks installed template identities.
Git-managed VMs require this pin; see [immutable Git inputs](stack-git-source.md#immutable-inputs-and-controller-ownership).
OCI digest syntax is checked locally; registry reference resolution and digest verification still need the host.
IPv6, uplink selection, VM shells, and standalone VM port forwards are outside this initial schema.

Host-local ownership and read-only `stack plan` are documented in [Stack ownership and planning](stack-planning.md).
Use [Native YAML stack apply](stack-apply.md) for explicit creation, changes, deployment and recovery.
[Git source synchronization](stack-git-source.md) supports GitHub polling, read-only drift detection, and opt-in native apply, including drift at unchanged commits.
Failed or interrupted Git applies require explicit recovery before automatic mutation continues.
