# REST API

`firecrab-api` manages images, networks, storage, and microVMs.
It listens on `127.0.0.1:3000` by default.

## Run the API

Run it from the repository root.
The default data paths use the current working directory.

```sh
cargo run -p firecrab-api
```

Set a more detailed log level when needed.

```sh
RUST_LOG=firecrab_api=debug cargo run -p firecrab-api
```

## Common rules

- JSON requests must use `Content-Type: application/json`.
- A request body can be at most 64 KiB.
- Every response has an `X-Request-Id` header.
- REST requests have a 10 second server deadline.
- At most 128 REST requests run at the same time.
- Unknown `/api` and `/ws` paths return a JSON error.

See [API errors](api-error.md) for the error body.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:3000` | HTTP listen address |
| `FIRECRAB_ALLOWED_ORIGINS` | `http://localhost:8080` in development | Allowed browser origins |
| `FIRECRAB_ENV` | empty | `production` disables the development origin default |
| `FIRECRAB_IMAGE_ROOT` | `images` resolved by the API | Kernel and rootfs directory |
| `FIRECRAB_IMAGE_BASE_URL` | empty | Base URL for M2Image packages |
| `FIRECRAB_FIRECRACKER_BIN` | `firecracker` from `PATH` | Firecracker executable |
| `FIRECRAB_STATIC_ROOT` | empty | Built dashboard directory |
| `FIRECRAB_STORAGE_ROOTS` | `default=data` | Colon-separated `id=path` storage roots |
| `FIRECRAB_NET_HELPER_SOCK` | `/run/firecrab/net-helper.sock` | Network helper socket |

A non-loopback bind requires both authentication and TLS flags.
The current flags are `FIRECRAB_AUTHENTICATION_ENABLED` and `FIRECRAB_TLS_ENABLED`.

## VM endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/vms` | List VMs |
| `POST` | `/api/vms` | Create a VM record |
| `GET` | `/api/vms/{id}` | Get one VM |
| `PUT` | `/api/vms/{id}` | Update inactive VM resources |
| `DELETE` | `/api/vms/{id}` | Delete an inactive VM and its disk |
| `POST` | `/api/vms/{id}/start` | Start a VM |
| `POST` | `/api/vms/{id}/stop` | Stop a VM |
| `GET` | `/api/vms/{id}/log` | Read the VM log |
| `POST` | `/api/vms/{id}/packages` | Run a guest package action |
| `PUT` | `/api/vms/{id}/storage` | Assign storage before a disk exists |
| `GET` | `/ws/vms/{id}/console` | Open the serial console WebSocket |

### Create a VM

Create a MicroNetwork before creating a VM.
There is no hidden default subnet.

```sh
NETWORK_ID=$(curl -s -X POST http://127.0.0.1:3000/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.30.0.0/24","internetEnabled":true}' \
  | jq -r '.id')

curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"demo\",
    \"template\": \"alpine-3.24\",
    \"cpu\": 1,
    \"ram\": 512,
    \"diskGb\": 2,
    \"microNetworkId\": \"$NETWORK_ID\",
    \"egressPolicy\": \"internet\"
  }"
```

| Field | Rule |
| --- | --- |
| `name` | 1 to 64 safe name characters |
| `template` | Installed image alias |
| `cpu` | 1 to 32 |
| `ram` | 128 to 32768 MiB and a power of two |
| `diskGb` | At least the image size and at most 500 GiB |
| `microNetworkId` | Existing MicroNetwork UUID |
| `egressPolicy` | `internet` or `isolated` |
| `storageRoot` | Optional ID from `GET /api/storage` |

The response status is `201 Created`.
The returned `id` is used by later operations.

### Start and stop

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/$VM_ID/start
curl -s -X POST http://127.0.0.1:3000/api/vms/$VM_ID/stop
```

Start is allowed from `created`, `stopped`, or `error`.
Stop is allowed from `running`.

Delete is allowed from `created`, `stopped`, or `error`.
Delete removes the VM record and local disk files.

## VM states

```text
created -> starting -> running -> stopping -> stopped
              |           |          |
              +-> error <-+----------+
```

An unexpected guest exit becomes `error`.
A clean guest shutdown becomes `stopped`.

## MicroNetwork endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/micro-networks` | List networks |
| `POST` | `/api/micro-networks` | Create a network |
| `GET` | `/api/micro-networks/{id}` | Get network details |
| `PATCH` | `/api/micro-networks/{id}` | Change the internet policy |
| `DELETE` | `/api/micro-networks/{id}` | Delete an unused network |

See the [MicroNetwork guide](explicit-micro-network.md).

## Storage endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/storage` | List all selectable storage roots |
| `GET` | `/api/storage/devices` | List mounted storage candidates |
| `GET` | `/api/micro-storages` | List registered pools |
| `POST` | `/api/micro-storages` | Register a mounted directory |
| `GET` | `/api/micro-storages/{id}` | Get a pool and its VMs |
| `DELETE` | `/api/micro-storages/{id}` | Delete an unused pool |

See the [MicroStorage guide](micro-storage.md).

## Image endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/images` | List known images |
| `DELETE` | `/api/images/{alias}` | Remove an unused image |
| `GET`, `POST` | `/api/images/{alias}/package` | Inspect or start package download |
| `DELETE` | `/api/images/{alias}/package` | Remove a staged package |
| `GET`, `POST` | `/api/images/{alias}/install` | Inspect or start image install |
| `POST` | `/api/images/{alias}/bootstrap` | Start a builder VM |
| `GET`, `DELETE` | `/api/images/bootstrap/{bootstrapId}` | Inspect or cancel bootstrap |

Package and image work can return `202 Accepted`.
Poll the matching `GET` endpoint for progress.

See the [M2Image guide](m2image-builder.md).

## Host endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/host` | Host health and capacity |
| `GET` | `/api/network` | Host network information |

## Data and files

SQLite state is stored in `data/firecrab.db` by default.
VM artifacts are stored below the selected storage root.

The disk generation is durable across stop and start.
Runtime configuration, sockets, and logs are created for each start.
