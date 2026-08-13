# API

`firecrab-api` provides REST endpoints and one console WebSocket.
It listens on `127.0.0.1:3000` by default.

## Run

Run the API from the repository root.

```sh
cargo run -p firecrab-api
```

Use `RUST_LOG=firecrab_api=debug` for detailed logs.

## Request rules

- JSON requests use `Content-Type: application/json`.
- Request bodies are limited to 64 KiB.
- Every response has `X-Request-Id`.
- REST requests have a 10 second deadline.
- Invalid paths return a JSON error.

## VM endpoints

| Method | Path | Job |
| --- | --- | --- |
| `GET`, `POST` | `/api/vms` | List or create VMs |
| `GET`, `PUT`, `DELETE` | `/api/vms/{id}` | Read, edit, or delete a VM |
| `POST` | `/api/vms/{id}/start` | Start a VM |
| `POST` | `/api/vms/{id}/stop` | Stop a VM |
| `GET` | `/api/vms/{id}/log` | Read logs |
| `PUT` | `/api/vms/{id}/storage` | Assign storage |
| `GET` | `/ws/vms/{id}/console` | Open the serial console |

## Create a VM

Create a MicroNetwork first.

```sh
NETWORK_ID=$(curl -s -X POST http://127.0.0.1:3000/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.30.0.0/24"}' \
  | jq -r '.id')
```

Create the VM with the returned network ID.

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"demo\",
    \"template\": \"alpine-3.24.1\",
    \"cpu\": 1,
    \"ram\": 512,
    \"diskGb\": 2,
    \"microNetworkId\": \"$NETWORK_ID\"
  }"
```

The response has status `201` and includes the VM UUID.

## VM fields

| Field | Rule |
| --- | --- |
| `name` | 1 to 64 safe name characters |
| `template` | Installed image alias |
| `cpu` | 1 to 32 |
| `ram` | 128 to 32768 MiB and a power of two |
| `diskGb` | Image minimum to 500 GiB |
| `microNetworkId` | Existing network UUID |
| `egressPolicy` | `internet` or `isolated` |
| `storageRoot` | Optional storage ID |
| `shellIds` | Optional Shell repository ids (latest revision pinned) |

## Other endpoints

| Resource | Paths |
| --- | --- |
| MicroNetwork | `/api/micro-networks` and `/{id}` |
| MicroStorage | `/api/storage`, `/api/storage/devices`, `/api/micro-storages` |
| Shells | `/api/shells`, `/{id}`, `POST /{id}/revisions`, `GET /{id}/revisions/{revisionId}`; VM pin `PUT /api/vms/{id}/shells` (Alpine OpenRC + Ubuntu/Rocky systemd; prefer POSIX `/bin/sh`) |
| Images | `/api/images`, `/package`, `/install`, `/bootstrap` |
| Host | `/api/host` and `/api/network` |

## VM states

```text
created -> starting -> running -> stopping -> stopped
              |           |          |
              +-> error <-+----------+
```

Start is allowed from `created`, `stopped`, and `error`.
Delete is allowed only while a VM is inactive.

## Errors

```json
{
  "error": {
    "code": "validation_failed",
    "message": "request validation failed",
    "fields": {},
    "requestId": "<uuid>"
  }
}
```

Common status codes are `400`, `404`, `409`, `413`, `415`, `429`, `500`, `503`, and `504`.
Use `requestId` to find the matching server log.

## Related

- [Networking](networking.md)
- [Storage](storage.md)
- [Images](images.md)
- [Troubleshooting](troubleshooting.md)
