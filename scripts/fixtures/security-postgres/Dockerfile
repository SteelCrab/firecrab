# Digests pin both multi-platform images used by the disposable QA.
FROM busybox:1.36.1-musl@sha256:3c6ae8008e2c2eedd141725c30b20d9c36b026eb796688f88205845ef17aa213 AS qa-toolbox
FROM postgres:17-alpine@sha256:266d3c257594e195b309ae9d1805e54c788f1d7b375f56bf6c367aac6c917e7b
COPY --from=qa-toolbox /bin/busybox /usr/local/bin/busybox
COPY --chmod=0755 run-qa /usr/local/bin/firecrab-postgres-qa
COPY --chmod=0755 health /qa-www/cgi-bin/health
ENV POSTGRES_INITDB_ARGS=--auth-host=scram-sha-256
ENTRYPOINT ["/usr/local/bin/firecrab-postgres-qa"]
CMD []
