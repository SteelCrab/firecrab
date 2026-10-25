# AI Sandbox UI captures

These Rust/WASM screenshots use the deterministic browser fixture in `firecrab-e2e/tests/ai-sandbox.spec.ts`.
The account, model, task result, test output, and project path are fixture data.
They demonstrate UI behavior, not live provider or MicroVM execution evidence.
The captures include main's Queue and MicroBlockStorage navigation entries and retain AI Sandbox below System.

- `conversation-ko.png`: Korean desktop conversation.
- `tasks-ko.png`: Korean desktop task board.
- `review-mobile-ko.png`: Korean mobile before/after review and selected-file apply.

Regenerate with `npm run test:sandbox --prefix firecrab-e2e` (set `FIRECRAB_SANDBOX_E2E_PORT` if the default port is occupied); copy the corresponding test output PNGs after visual review.
