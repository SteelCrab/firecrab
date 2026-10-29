ARG DISTROLESS_IMAGE
FROM ${DISTROLESS_IMAGE}
COPY probe /usr/local/bin/firecrab-qa-probe
ENTRYPOINT ["/usr/local/bin/firecrab-qa-probe"]
