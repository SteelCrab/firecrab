import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Plugin } from "vite";

// These exact edits target the locked ghostty-web 0.4.0 distribution. Fail a
// dependency upgrade explicitly rather than silently losing the optimizations.
const originalResize = `  resize(A, B) {
    if (this.assertOpen(), A === this.cols && B === this.rows)
      return;
    this.cols = A, this.rows = B, this.wasmTerm.resize(A, B), this.renderer.resize(A, B);
    const g = this.renderer.getMetrics();
    this.canvas.width = g.width * A, this.canvas.height = g.height * B, this.canvas.style.width = \`\${g.width * A}px\`, this.canvas.style.height = \`\${g.height * B}px\`, this.resizeEmitter.fire({ cols: A, rows: B }), this.renderer.render(this.wasmTerm, !0, this.viewportY, this);
  }`;

const optimizedResize = `  resize(A, B) {
    if (this.assertOpen(), A === this.cols && B === this.rows)
      return;
    this.cols = A, this.rows = B, this.wasmTerm.resize(A, B);
    this.resizeEmitter.fire({ cols: A, rows: B });
    // The existing animation loop sizes the canvas and draws next frame.
    // Preserve every core resize and event; skip only invisible intermediate draws.
  }`;

export function ghosttyOptimizations(): Plugin {
  return {
    name: "firecrab-ghostty-optimizations",
    enforce: "pre",
    config: () => ({ optimizeDeps: { exclude: ["ghostty-web"] } }),
    transform(source, id) {
      const filename = id.split("?")[0];
      if (!filename.replaceAll("\\", "/").endsWith("/ghostty-web/dist/ghostty-web.js")) return;
      const embedded = source.match(/"data:application\/wasm;base64,[A-Za-z0-9+/=]+"/g);
      if (embedded?.length !== 1 || source.split(originalResize).length !== 2) {
        throw new Error("ghostty-web 0.4.0 source changed; review the WASM and resize patches before upgrading");
      }
      const wasm = readFileSync(join(dirname(filename), "ghostty-vt.wasm"));
      const inline = Buffer.from(embedded[0].slice('"data:application/wasm;base64,'.length, -1), "base64");
      if (!wasm.equals(inline)) throw new Error("Ghostty's inline and external WASM artifacts differ");
      return {
        code: 'import firecrabGhosttyWasmUrl from "./ghostty-vt.wasm?url";\n'
          + source.replace(embedded[0], "firecrabGhosttyWasmUrl").replace(originalResize, optimizedResize),
        map: null,
      };
    },
  };
}
