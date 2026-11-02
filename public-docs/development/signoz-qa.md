# SigNoz integration QA

## Automated checks

From the repository root:

```sh
cargo test -p firecrab-api --locked observability
cargo test -p firecrab-api --locked otlp
cargo test -p firecrab-api --locked service_observation
cargo clippy -p firecrab-api --all-targets --locked -- -D warnings
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
npm test --prefix firecrab-frontend
python3 scripts/check-product-versions.py
python3 scripts/check-changelog.py
python3 scripts/check-doc-links.py
```

The full native API suite requires the Linux tools and filesystem behavior
provided by CI. Native macOS results are not a replacement for that suite.

## Real SigNoz acceptance

Use a disposable local SigNoz stack with its ClickHouse HTTP interface exposed
only on loopback. Use separate ports and an explicit Compose project name.
Do not point this test at an existing installation.

```sh
FIRECRAB_SIGNOZ_QA_ENDPOINT=http://127.0.0.1:25878 \
FIRECRAB_SIGNOZ_QA_QUERY_ENDPOINT=http://127.0.0.1:25876 \
cargo test -p firecrab-api --locked \
  live_signoz_accepts_selected_records_pause_resume_failure_recovery_and_disconnect \
  -- --ignored --nocapture
```

The test creates a temporary Firecrab database and two service definitions.
It seeds test-generated observation records, then uses the production settings,
exporter, pause/resume, test-log, and disconnect paths. It checks actual indexed
SigNoz rows through ClickHouse. It does not boot a MicroVM or replace guest
log-collection acceptance.

The check requires:

- Selected-service stdout and stderr: two indexed records.
- Unselected-service output: zero indexed records.
- Output from before opt-in: zero indexed records.
- A lifecycle event and a record queued during pause: one indexed record each.
- A local fault proxy: three attempts on HTTP 503, a failed batch in history,
  and a later HTTP 200 after recovery.
- Disconnect: no active exporter or persisted integration.

## Verified local run

The acceptance test passed on 2026-11-02 against a disposable Docker stack
rendered by SigNoz Foundry v0.4.0. The collector ran with its static configuration.
This avoided the generated OpAMP endpoint selecting the PostgreSQL service.

Images used:

- `signoz/signoz@sha256:a7d1a968a1f7f65c518106150ac05424ca8e91c52d7076077dfce034980e9c34`
- `signoz/signoz-otel-collector@sha256:3996e0ffc58bc86d433062a45ef7ff960a4768da0619944f651a17254d634daf`
- ClickHouse server and keeper: `25.12.5`.

Observed sink counts were `2, 0, 0, 1, 1` for selected output, unselected output,
pre-opt-in output, lifecycle event, and resumed output respectively.
Keep the stack's containers, network, and volumes isolated, and remove them
after collecting evidence.

## Actual MicroService guest output

The Linux installer CI job starts the pinned stack in
`scripts/fixtures/signoz/compose.yaml`, runs the existing disposable Deploy QA,
and removes only that Compose project's containers and volumes.
The fixture uses the SigNoz collector, ClickHouse, Keeper, PostgreSQL, and SigNoz
query/UI service; only the log ingestion pipeline is enabled.
All published ports bind to loopback.

On an empty disposable Linux KVM host with the built Firecrab services installed:

```sh
docker compose -p firecrab-signoz-qa \
  -f scripts/fixtures/signoz/compose.yaml up -d --wait --wait-timeout 180
sudo python3 scripts/ci-qa-deploy.py --disposable-host --observability \
  --signoz-endpoint http://127.0.0.1:25878 \
  --signoz-query-endpoint http://127.0.0.1:25876 \
  --report /tmp/firecrab-signoz-guest.json
```

The test boots actual MicroServices and collects their guest output through the
production observation path before exporting it through the central integration.
It requires one indexed startup stdout and stderr record from the selected
source, a lifecycle event, zero pre-opt-in records, and zero records from the
unselected source.
Unique CGI output identifies batches for collector failure/recovery,
pause/resume, and disconnect.
An explicit test log must also appear in the SigNoz log table.
Disconnect must leave previously indexed records intact.

The JSON report contains `observability.signoz` stages and measured counts.
CI retains this report, the Deploy log, and `signoz.log`.
After a local run, retain the logs and always remove this owned fixture:

```sh
docker compose -p firecrab-signoz-qa \
  -f scripts/fixtures/signoz/compose.yaml logs --no-color > /tmp/signoz.log
docker compose -p firecrab-signoz-qa \
  -f scripts/fixtures/signoz/compose.yaml down --volumes --remove-orphans
```

The earlier test-generated-record acceptance remains a separate, faster check.
It does not replace this guest-output acceptance.
