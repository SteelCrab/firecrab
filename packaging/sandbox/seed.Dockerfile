# syntax=docker/dockerfile:1
# Build from an operator-recorded Alpine digest for the QA host architecture.
ARG ALPINE_IMAGE
FROM ${ALPINE_IMAGE}
RUN apk add --no-cache python3 openrc openssh-server openssh-client util-linux tar coreutils
COPY scripts/fixtures/sandbox-environment-seed.py /usr/local/libexec/firecrab-sandbox-seed
COPY scripts/fixtures/sandbox-environment-seed.json /usr/share/firecrab/qa-seed-manifest.json
RUN python3 /usr/local/libexec/firecrab-sandbox-seed --init-only
EXPOSE 8080 5432
CMD ["/usr/bin/python3", "/usr/local/libexec/firecrab-sandbox-seed", "--role", "auto"]
