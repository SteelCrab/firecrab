# Benchmarks

The Benchmark Control Center starts host-local `firecrab-bench` jobs and displays their results.

## Contents

- [Control Center](#control-center)
- [Safety](#safety)
- [API](#api)
- [CLI](#cli)
- [Development mode](#development-mode)
- [Installation](#installation)
- [Related](#related)

## Control Center

Open `http://127.0.0.1:15523/`.

The dashboard provides Boot, Concurrent Create, Maximum Density, and Lifecycle Stress commands.

Each command selects an installed image, a MicroNetwork, and VM resource limits.

The page polls job state every two seconds and benchmark results every fifteen seconds.

Completed jobs publish their common JSON result into benchmark history.

## Safety

Only one benchmark job runs at a time.

Dashboard limits are 100 boot samples, 100 concurrent creates, 100 density VMs, and 1,000 lifecycle iterations.

Maximum Density requires an explicit host-load acknowledgement.

Cancellation terminates the child benchmark process.

Recent job state and logs are in memory and reset when `firecrab-api` restarts.

## API

| Method | Path | Result |
| --- | --- | --- |
| `GET` | `/api/benchmark-jobs` | Recent jobs |
| `POST` | `/api/benchmark-jobs` | Start one job |
| `GET` | `/api/benchmark-jobs/{id}` | Job status and log |
| `DELETE` | `/api/benchmark-jobs/{id}` | Cancel an active job |
| `GET` | `/api/benchmarks` | Stored benchmark results |

`FIRECRAB_BENCH_BIN` overrides the benchmark executable path.

`FIRECRAB_BENCH_API` overrides the API base used by the child and defaults to `http://127.0.0.1:5523`.

## CLI

Run a small API benchmark and publish its result.

```sh
/usr/local/lib/firecrab/firecrab-bench --publish api --requests 100 --concurrency 10
```

Use the CLI directly for Network, Storage, Soak, Leak, and Regression commands not exposed by the MVP Control Center.

## Development mode

Run every command from the repository root.
Development mode uses three terminals and does not require a host installation.

Start the privileged helper in terminal 1.
The script builds the current debug helper before invoking `sudo`.

```sh
./scripts/dev-net-helper.sh
```

Start the API and benchmark executable in terminal 2.

```sh
cargo build -p firecrab-bench
FIRECRAB_BENCH_BIN="$PWD/target/debug/firecrab-bench" cargo run -p firecrab-api
```

Start Vite in terminal 3.

```sh
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

Open `http://localhost:8080/#/benchmarks`.

| Listener | Development use |
| --- | --- |
| `127.0.0.1:5523` | REST API and Vite proxy target |
| `127.0.0.1:15523` | API benchmark listener; built dashboard only when `FIRECRAB_STATIC_ROOT` is set |
| `localhost:8080` | Vite development dashboard |

Confirm the API can find the benchmark executable before starting a job.

```sh
test -x target/debug/firecrab-bench
curl -s http://127.0.0.1:5523/api/benchmark-jobs
```

## Installation

Local installation requires all four host binaries.

```sh
cargo build --release \
  -p firecrab-api -p firecrab-net-helper -p firecrab-cli -p firecrab-bench
npm run build --prefix firecrab-frontend
./install.sh --no-deps --bin-dir target/release --dashboard-dir firecrab-frontend/dist
```

## Related

- [Dashboard](dashboard.md)
- [API](api.md)
- [Installation](installation.md)
- [Operations](operations.md)
