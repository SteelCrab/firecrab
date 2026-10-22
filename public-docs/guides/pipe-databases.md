# PIPE database connections

Open the **Database** card (`#/pipe/database`) and select **Add database**.
Enter a connection name and choose **Local DB URI**, **Supabase**, or **Amazon RDS**. Paste a database URI:

```text
mysql://user:password@host:3306/database
postgresql://user:password@host:5432/database
```

`postgres://` is also accepted. Percent-encode reserved characters in passwords.
Use **Test connection** to authenticate without saving. **Save connection** tests
the connection again, then saves it. Nginx and tunnel installation are not required.

For [Supabase](https://supabase.com/docs/guides/database/connecting-to-postgres),
copy the PostgreSQL connection URI from **Connect**. Use the session pooler for
IPv4-only networks. A project HTTPS URL or Supabase API key is not a DB URI.
For [Amazon RDS](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/CHAP_CommonTasks.Connect.html),
use a MySQL or PostgreSQL endpoint and database credentials. Its security group
must allow the Firecrab API host to reach the database port.

Connections originate from the API host. `localhost` on macOS and Windows means
the managed Linux guest, not the desktop host. Use a desktop address reachable
from that guest to connect to a database on the desktop.

Open **TLS & certificates** to change TLS or add a CA certificate. These optional
settings are collapsed by default. TLS defaults to disabled for local URI
connections and certificate/hostname verification for Supabase and RDS.
The URI may set `sslmode` or `ssl-mode` to
`disable`, `require`, or `verify-full` (MySQL also accepts `DISABLED`, `REQUIRED`,
and `VERIFY_IDENTITY`). An explicit TLS selector overrides the URI. Cloud
connections cannot disable TLS. Paste a PEM CA bundle into **CA certificate**
when verification requires it, such as an RDS CA bundle. File paths, Unix sockets,
and other URI query parameters are not accepted.

Select **Browse**, then choose a table. The preview uses generated SELECT queries
and never accepts SQL input. Use an account with SELECT permission. Up to 500
base tables are listed. Each page contains up to 50 rows, with at most 128
columns and 2048 characters per cell. Binary MySQL values are shown as hex;
PostgreSQL uses its native text representation. NULL remains distinct from an
empty string. Preview data is not saved. Row order can change between pages.
Responses stop at approximately 2 MiB of row data. Database operations time out
after eight seconds. **Refresh rows** retries the selected table.

The API stores connection credentials in its existing owner-only SQLite file.
URI passwords and CA certificates never appear in list or save responses.
Open the connection's **⋯** menu and select **Remove connection** to delete only
the saved connection. It does not change the source DB.
Existing connections survive API restarts.

| Endpoint | Action |
| --- | --- |
| `GET /api/pipe/databases` | List saved connection metadata without credentials |
| `POST /api/pipe/databases` | Test and save `{name, provider, uri, tlsMode?, caCertificate?}` |
| `POST /api/pipe/databases/test` | Test an unsaved connection |
| `DELETE /api/pipe/databases/{id}` | Remove one saved connection |
| `GET /api/pipe/databases/{id}/tables` | List accessible base tables |
| `POST /api/pipe/databases/{id}/preview` | Read `{schema, table, limit?, offset?}`; limit 1–100, offset 0–100000 |

`provider` is `local`, `supabase`, or `rds`. `tlsMode` is `disable`, `require`, or
`verify_full`. A Supabase connection requires PostgreSQL. RDS supports both engines.
The DB routes use the standard API origin, body-size, concurrency and deadline limits.

## Validation

See [PIPE database E2E and QA](../development/pipe-databases-qa.md) for browser,
real API/DB, TLS, cleanup and cloud acceptance checks.

- `cargo test -p firecrab-api --bin firecrab-api pipe_databases`: URI/provider/TLS
  validation, credential redaction, persisted connections, API origin checks and
  bounded operations. The ignored `reads_real_postgres_and_mysql_tables` test
  requires disposable databases in `FIRECRAB_TEST_POSTGRES_URI` and
  `FIRECRAB_TEST_MYSQL_URI`; it creates fixture tables in those databases.
- `npm run test:pipe --prefix firecrab-e2e`: existing PIPE flows plus DB connection
  tests, table previews, pagination, failure recovery, credential clearing and
  English mobile layout using API fixtures.
