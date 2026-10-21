# PIPE database screenshots

Captured on 2026-10-21 from the DB connection implementation represented by
`1cd688d93e8105c9cd2aee294ea97e69b1a46b54` in the original development branch.
The source QA report recorded its pre-commit base as
`85a08ac72cd3fe2b6163c4dd472907cec68fa245` with uncommitted DB changes.
The feature commits are applied to current main in this PR.

| File | Scenario |
| --- | --- |
| `postgres-desktop.png` | Real PostgreSQL 17.11 read-only preview on desktop |
| `postgres-mobile.png` | Real PostgreSQL 17.11 read-only preview on Pixel 7 |
| `mysql-desktop.png` | Real MariaDB 11.8.6 (MySQL-compatible) preview on desktop |
| `mysql-mobile.png` | Real MariaDB 11.8.6 (MySQL-compatible) preview on Pixel 7 |
| `database-form-mobile-ko.png` | Korean RDS URI/TLS/CA form using API fixtures, 390×844 viewport |

Preview values, connection names and table names are disposable QA fixtures.
The screenshots contain no database passwords, CA certificates or live cloud credentials.

The captured QA run passed 74 fixture browser cases (22 DB cases), four real
browser cases, 16 real API QA cases and five QA runner contract tests.
The Docker MySQL 8.4 workflow and live Supabase/RDS account checks were not run
on the screenshot host. See the [QA guide](../../../public-docs/development/pipe-databases-qa.md).
