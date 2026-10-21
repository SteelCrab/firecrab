# PIPE database E2E and QA

Use disposable loopback databases. The QA runner starts its own API with a fresh
SQLite file and an absent helper socket. It does not contact the installed API,
change systemd units, or use existing VM state. Fixture tables have random
`qa_pipe_` names and are removed after success or failure.

## Browser fixtures

```sh
npm run typecheck:pipe --prefix firecrab-e2e
npm run test:pipe:databases --prefix firecrab-e2e
npm run test:pipe --prefix firecrab-e2e
```

The database suite has 22 cases. The complete PIPE suite has 74 cases. These
tests mock DB API responses and need no Linux API, DB server, helper, or KVM.
Install frontend and E2E dependencies and Chromium first, as described in the
[browser E2E guide](../../firecrab-e2e/README.md). Port 8096 must be free.
Override it with `FIRECRAB_PIPE_E2E_BASE_URL` if necessary.

## Real API, databases, TLS and browser

On Linux with Docker, OpenSSL, Python 3, `psql`, and `mysql`:

```sh
cargo build -p firecrab-api --locked
npm run build --prefix firecrab-frontend
bash scripts/ci-qa-pipe-databases.sh --browser
```

This starts PostgreSQL 17 and MySQL 8.4 containers with randomly assigned
loopback ports and temporary data files. It generates a temporary CA and server
certificates with localhost/IP subject names. It verifies trusted certificates
and rejection without the CA. It runs HTTP QA and four real browser cases:
desktop and mobile for each engine. DB requests are not mocked in these cases.
Container cleanup runs on success, failure, and interrupt. It removes only the
containers created by this run. Docker image layers may remain cached.

The `pipe-databases` CI job runs this command. It retains the JSON report,
redacted API log, browser screenshots, and failure traces for seven days.

## Existing disposable fixtures

The Python runner can also use separately prepared test DBs. Supply password
authentication and fixture accounts that can create, read and drop tables in the
selected database. PostgreSQL uses the `public` schema. The URIs must use
`localhost`, `127.0.0.1`, or `::1`. Remote database hosts are rejected before SQL.

Set `FIRECRAB_TEST_POSTGRES_URI` and `FIRECRAB_TEST_MYSQL_URI` in the environment.
Use long, unique test passwords. Do not put URIs in shell history or command-line
arguments. For TLS checks, also set `FIRECRAB_TEST_POSTGRES_CA` and
`FIRECRAB_TEST_MYSQL_CA` to PEM CA files. Then run:

```sh
python3 scripts/test-pipe-databases.py --api target/debug/firecrab-api \
  --require-tls --output target/qa/pipe-databases.json
```

Add `--browser --frontend-dist firecrab-frontend/dist` to include real browser
checks. Without CA files, omit `--require-tls`; certificate checks are reported
as `WARNING`. Missing databases or clients fail the run. No successful check is
reported for missing prerequisites.

## Case matrix

| Case | Automated evidence |
| --- | --- |
| PDB1: local MySQL/PostgreSQL, Supabase and RDS input | Browser fixtures; real local engines; TLS provider requests |
| PDB2: incomplete input, URI masking, browser storage | Browser fixtures |
| PDB3: TLS selection, PEM forwarding and source switch | Browser fixtures; real TLS with trusted and missing CA |
| PDB4: failed authentication and validation | Real HTTP statuses and no persisted failed connection |
| PDB5: tables, NULL, empty text, Unicode, binary values | Real API against both engines; real desktop/mobile preview |
| PDB6: quoted identifiers and HTML values | Real quoted table lookup; browser literal text rendering |
| PDB7: pagination and cell/column limits | Real HTTP page boundaries, 2048-character cells, 129-column rejection; compact scrollable mobile cells |
| PDB8: unknown SQL field, invalid limits, origin and URI file parameters | Real HTTP rejection; source fingerprint unchanged |
| PDB9: restart persistence and credential exposure | Real API restart, response/log inspection and SQLite mode 0600 |
| PDB10: stale rows, errors, retry and duplicate requests | Browser fixtures with held requests and injected failures |
| PDB11: draft navigation and cancellation | Browser fixtures with real browser dialogs |
| PDB12: delete, cleanup and source preservation | Real API removal, 404 after removal, source fingerprint unchanged |

## Manual cloud acceptance

The local TLS fixtures test the provider connection path. They do not establish
access to an actual Supabase project or Amazon RDS instance. Record these checks
as `WARNING` until they are completed with a dedicated cloud test database:

1. Connect a Supabase direct URI and session-pooler URI on the available IP network.
2. Connect RDS PostgreSQL and RDS MySQL with the matching RDS CA bundle.
3. Reject an incorrect password, CA and inaccessible security-group configuration.
4. Read a table, restart the API, read it again, and remove the connection.
5. Confirm that source records remain unchanged and credentials are absent from responses/logs.

Retain source revision, dirty state, platform, command, exit code and artifact
paths. The JSON report contains these fields. An exported source tree can supply
`FIRECRAB_QA_SOURCE_REVISION` and `FIRECRAB_QA_SOURCE_DIRTY` (`0` or `1`).

## Runner cleanup contracts

```sh
python3 scripts/test-pipe-databases-qa.py
```

These tests use fake Docker commands to check remote-host refusal, cleanup after
container/TLS failures, and propagation of browser/QA failures. They do not prove
that Docker images boot; the real CI job supplies that evidence.
