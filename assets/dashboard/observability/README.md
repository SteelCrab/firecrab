# Observability dashboard captures

These captures show the implemented React dashboard using its deterministic
browser fixture. Service names, endpoints, counters, and delivery responses in
the images are fixture data. They demonstrate layout, not live ingestion.

- `desktop-en.png`: English integration overview.
- `settings-en.png`: English connection settings drawer.
- `mobile-ko.png`: Korean overview at a 390px width.

Behavior is covered by `firecrab-frontend/tests/observability-ui.test.mjs`.
Real SigNoz ingestion is covered separately by the opt-in acceptance test in
`firecrab-api/src/observability/tests.rs`.
