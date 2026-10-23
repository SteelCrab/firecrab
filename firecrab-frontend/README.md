# firecrab dashboard

React and TypeScript UI for firecrab.
Develop it from the repository root as described in [README.md](../README.md).
OCI inspect → import browser tests live in [firecrab-e2e](../firecrab-e2e/README.md).

The Pools screen asks for a name and image. Network and resource settings use
defaults and remain available under Advanced settings. Use VM opens a fresh
session; End session deletes its VM and prepares a replacement automatically.


## WASM console

The serial console pins `ghostty-web` 0.4.0-next.20.g1858a59. This upstream
prerelease fixes resize/high-output failures found in 0.4.0. Ghostty's WASM core owns VT parsing,
terminal modes, Unicode cells, reflow and history. TypeScript connects browser
input, Canvas rendering and the existing console WebSocket. There is no xterm
runtime or engine switch.

Compiled WASM code is cached; each session gets a separate memory and terminal.
Output is processed in 16 KiB chunks with a 4 ms scheduling budget and an 8 MiB
pending cap. History has an 8 MiB byte budget: the pinned package's `scrollback`
option is bytes despite its documentation ([upstream #140](https://github.com/coder/ghostty-web/issues/140)).
Session isolation avoids [shared-memory corruption](https://github.com/coder/ghostty-web/issues/141).
Text export reads native graphemes to avoid [truncated clusters](https://github.com/coder/ghostty-web/issues/200).

`build/ghosttyOptimizations.ts` verifies the pinned source and WASM bytes before
externalizing the binary and correcting resize, DPI and selection behavior.
Review its checks when upgrading. MIT notices ship in `terminal-licenses.txt`.

The browser adapter supports composition/mobile editing, bracketed paste,
application cursor keys, selection copy, history paging, SGR/legacy mouse and
focus reports. Live cursor/status queries receive native replies; the API strips
old queries from replay. Shift overrides application mouse capture for text selection.
The existing serial transport does not propagate the browser's grid to a guest
PTY. Real guest TUI resizing and actual OS IMEs require separate host validation.
See [browser E2E](../firecrab-e2e/README.md#wasm-console) for local checks.

---

# React + TypeScript + Vite

This template provides a minimal setup to get React working in Vite with HMR and some Oxlint rules.

Currently, two official plugins are available:

- [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react) uses [Oxc](https://oxc.rs)
- [@vitejs/plugin-react-swc](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react-swc) uses [SWC](https://swc.rs/)

## React Compiler

The React Compiler is not enabled on this template because of its impact on dev & build performances. To add it, see [this documentation](https://react.dev/learn/react-compiler/installation).

## Expanding the Oxlint configuration

If you are developing a production application, we recommend enabling type-aware lint rules by installing `oxlint-tsgolint` and editing `.oxlintrc.json`:

```json
{
  "$schema": "./node_modules/oxlint/configuration_schema.json",
  "plugins": ["react", "typescript", "oxc"],
  "options": {
    "typeAware": true
  },
  "rules": {
    "react/rules-of-hooks": "error",
    "react/only-export-components": ["warn", { "allowConstantExport": true }]
  }
}
```

See the [Oxlint rules documentation](https://oxc.rs/docs/guide/usage/linter/rules) for the full list of rules and categories.
