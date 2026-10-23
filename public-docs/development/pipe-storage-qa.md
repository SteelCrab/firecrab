# PipeStorage QA

The deterministic tests use loopback HTTP fixtures and private temporary SQLite
databases. The guest-file test uses a real ext4 image through `debugfs`; it does
not boot a MicroVM.

## Local checks

```sh
cargo fmt --all -- --check
cargo test -p firecrab-api pipe_storage --locked
cargo test -p firecrab-api s3_guest_credentials --locked
cargo test -p firecrab-api pipe_auth --locked
cargo test -p firecrab-api micro_services --locked
cargo test -p firecrab-api service_runtime --locked
cargo clippy -p firecrab-api --all-targets --locked -- -D warnings
cargo test -p firecrab-cli -p firecrab-api-types --locked
python3 scripts/test_pipe_storage_qa.py
npm run build --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run typecheck:pipe --prefix firecrab-e2e
```

The API requires Linux. Run it in a disposable Linux environment. Keep its
build target inside the Linux filesystem when host file sharing stalls linking.

Mocked browser coverage uses the separate Pipe and service configurations:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 node firecrab-e2e/node_modules/@playwright/test/cli.js test \
  --config firecrab-e2e/playwright.pipe.config.ts pipe-storage.spec.ts pipe.spec.ts
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 node firecrab-e2e/node_modules/@playwright/test/cli.js test \
  --config firecrab-e2e/playwright.mocked.config.ts micro-services.spec.ts
```

## Real AWS checks

Start a disposable API with an empty private data directory and loopback-only
access. Serve the built dashboard through `FIRECRAB_STATIC_ROOT`. Install the AWS
CLI on the machine running the QA script. Select the intended local AWS profile
before starting.

```sh
python3 scripts/test-pipe-storage-aws.py \
  --api http://127.0.0.1:15539 --isolated-api \
  --region ap-northeast-2 --cli /path/to/test/firecrab --browser \
  --output target/qa/s3-live.json
```

The setup profile creates a uniquely named bucket, IAM user, and IAM role tagged
with issue 439 and a random run ID. The user can assume only the test role.
The trust policy restricts the principal to that user's ARN. The role can access
only the test bucket's `qa/` prefix. A second role session further restricts
operations to list/read. The API receives role session credentials, including
their session token. The bootstrap user key stays in memory.

The setup profile needs permission to create/tag/delete these test resources,
manage their inline policies and access keys, and delete test objects. The test
user needs only `sts:AssumeRole` for the generated role. See
[AssumeRole permissions](https://docs.aws.amazon.com/STS/latest/APIReference/API_AssumeRole.html).

The runner checks:

- Scoped temporary credentials without ECR or account-wide bucket listing.
- Listing, opaque pagination, prefix restrictions, and protected overwrites.
- Multi-megabyte binary roundtrips and SHA-256 equality through REST and CLI.
- Real object names through the AWS CLI as well as Firecrab, including Korean,
  spaces, literal `+`, `%`, and `?`.
- Read-only downloads and denied uploads/deletes.
- Rejected credential replacement preserving the saved connection.
- Service references blocking disconnect, followed by data preservation.
- A real dashboard list/download, denied mutations, and desktop/mobile layouts.

Reports contain resource identifiers and results, with no signing material.
Screenshots are written under `target/qa/storage-live-browser`. The live-browser
configuration disables HTTP traces and automatic screenshots because account
authentication submits actual signing material. Explicit screenshots run only
after password fields have been removed.

## KVM and local-account acceptance

Use a dedicated Linux API with read/write access to `/dev/kvm`. Isolate its
network and mount namespaces: the helper uses fixed Firecrab firewall
table names and `/run/firecrab` files. A separate data directory alone does not
isolate those host operations. Use the real systemd VM launcher, with root-owned
API/helper executables in a trusted path. VM units must join the QA network
namespace through `NetworkNamespacePath`; keep their UUIDs for cleanup. The
API/helper must retain access to the real systemd bus. Keep the API loopback-only and use SSH forwarding
for the API and application ports when driving tests from another machine.

Prepare a private OCI image using the official amd64 Python Alpine layers and
the included boto3 application:

```sh
python3 -m pip install --target /path/to/private/vendor boto3
python3 scripts/prepare-pipe-storage-qa-image.py \
  --vendor /path/to/private/vendor --output /path/to/private/registry
python3 scripts/prepare-pipe-storage-qa-image.py \
  --output /path/to/private/registry --serve
```

The registry binds to `127.0.0.1:15439`; run it inside the isolated API network.
It publishes no image externally and includes no credentials. The application
uses boto3's environment provider, plus `FIRECRAB_S3_BUCKET` and
`FIRECRAB_S3_PREFIX`. The API and MicroVM must use the real Firecracker backend.

```sh
python3 scripts/test-pipe-storage-aws.py \
  --api http://127.0.0.1:15539 --isolated-api --region ap-northeast-2 \
  --microvm-image 127.0.0.1:15439/qa/s3:latest \
  --microvm-url http://127.0.0.1:15540 --browser --local-aws-auth \
  --output target/qa/s3-kvm-live.json
```

Forward application port `28080` to local `15540`. The runner checks SDK
read/write, the same VM's stop/start with a different Linux boot ID, and a
replacement VM with a different VM/boot ID reading the retained S3 object.
MicroVMs receive only the bucket-scoped role session.

`--local-aws-auth` additionally authenticates the selected local AWS profile
through the actual Auth form. It creates a second S3 connection referencing
that shared authentication, then uploads, downloads, compares and deletes a
test file in the dashboard. Credentials travel in process memory over the SSH
tunnel and are not saved in test files, images, screenshots or HTTP traces.
The temporary API database is private and must be deleted after cleanup.
Account screenshots are under `target/qa/storage-account-browser`.

## Cleanup before removing a worktree

The runner removes connections and services from its disposable API, then deletes
test objects, the bucket, the role, the user, their inline policies, and the
bootstrap access key in `finally`. API failures cannot skip cloud cleanup.

After a killed process or an interrupted terminal, inspect the resource ledger:

```sh
python3 scripts/test-pipe-storage-aws.py --cleanup target/qa/s3-resources.json
```

Cleanup requires the recorded account and issue-439 resource names. It refuses
unrelated names and unexpected data outside `qa/`. Confirm `cleaned: true` before
removing the worktree. Then stop the disposable API and remove its private data
directory so its SQLite database and journal no longer retain session credentials.
Remove test containers and build caches created for that worktree.

Real MicroVM acceptance is separate. A KVM-capable isolated Linux host must boot
a bound service, verify application S3 read/write through the injected environment,
and prove stop/start and replacement preserve external data. Fixture tests and
the ext4-file test do not establish that live runtime result.
