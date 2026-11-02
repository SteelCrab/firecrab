# Disposable SigNoz acceptance fixture

This is a test fixture for issue #579, not a product installation profile.
Use the explicit Compose project `firecrab-signoz-qa` and remove its volumes
after acceptance.

Images are pinned to the digests recorded during the original local acceptance.
Keeper and ClickHouse settings are reduced from the SigNoz Foundry v0.4.0
generated Compose installation used for that run.
The collector has a static logs-only configuration; no OpAMP endpoint or
external histogram download is needed for this test.

Loopback ports are 25878 for OTLP/HTTP, 25876 for ClickHouse test queries,
and 25879 for the SigNoz UI.
The PostgreSQL password is disposable fixture data.
No existing installation or credentials are used.

See [the QA guide](../../../public-docs/development/signoz-qa.md) for execution,
evidence collection, and cleanup.
