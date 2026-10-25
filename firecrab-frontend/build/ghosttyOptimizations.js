import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";

// Exact edits for the locked upstream revision; dependency upgrades require review.
const version = "0.4.0-next.20.g1858a59";
const originalResize = `  resize(A, Q) {
    if (this.assertOpen(), !(A === this.cols && Q === this.rows)) {
      this.cancelRenderLoop();
      try {
        this.cols = A, this.rows = Q, this.wasmTerm.resize(A, Q), this.renderer.resize(A, Q);
        const g = this.renderer.getMetrics();
        this.canvas.width = g.width * A, this.canvas.height = g.height * Q, this.canvas.style.width = \`\${g.width * A}px\`, this.canvas.style.height = \`\${g.height * Q}px\`, this.resizeEmitter.fire({ cols: A, rows: Q }), this.renderer.render(this.wasmTerm, !0, this.viewportY, this);
      } catch (g) {
        console.error("Terminal resize failed:", g);
      }
      this.flushWriteQueue(), this.startRenderLoop();
    }
  }`;
const optimizedResize = `  resize(A, Q) {
    this.assertOpen();
    if (A === this.cols && Q === this.rows) return;
    this.cancelRenderLoop();
    try {
      this.cols = A, this.rows = Q, this.wasmTerm.resize(A, Q);
      this.resizeEmitter.fire({ cols: A, rows: Q });
    } finally {
      this.flushWriteQueue();
      if (!this.isDisposed && this.isOpen) {
        this.animationFrameId = requestAnimationFrame(() => {
          this.animationFrameId = undefined;
          if (!this.isDisposed && this.isOpen) this.startRenderLoop();
        });
      }
    }
  }`;
const originalRender =
  "    this.currentBuffer = A;\n    const I = A.getCursor(), D = A.getDimensions()";
const scaledRender =
  "    this.devicePixelRatio = window.devicePixelRatio || 1;\n" +
  originalRender;
const originalSelectionCell =
  "        const t = o[k];\n        if (t && t.codepoint !== 0)";
const wideSelectionCell =
  "        const t = o[k];\n        if (t && t.width === 0) continue;\n        if (t && t.codepoint !== 0)";

export function ghosttyOptimizations() {
  return {
    name: "firecrab-ghostty-optimizations",
    enforce: "pre",
    config: () => ({ optimizeDeps: { exclude: ["ghostty-web"] } }),
    transform(source, id) {
      const filename = id.split("?")[0];
      if (
        !filename
          .replaceAll("\\", "/")
          .endsWith("/ghostty-web/dist/ghostty-web.js")
      )
        return;
      const embedded = source.match(
        /"data:application\/wasm;base64,[A-Za-z0-9+/=]+"/g,
      );
      if (
        embedded?.length !== 1 ||
        [originalResize, originalRender, originalSelectionCell].some(
          (pattern) => source.split(pattern).length !== 2,
        )
      ) {
        throw new Error(
          `ghostty-web ${version} changed; review the terminal build patches before upgrading`,
        );
      }
      const wasm = readFileSync(join(dirname(filename), "ghostty-vt.wasm"));
      const inline = Buffer.from(
        embedded[0].slice('"data:application/wasm;base64,'.length, -1),
        "base64",
      );
      if (!wasm.equals(inline))
        throw new Error("Ghostty's inline and external WASM artifacts differ");
      return {
        code:
          'import firecrabGhosttyWasmUrl from "./ghostty-vt.wasm?url";\n' +
          source
            .replace(embedded[0], "firecrabGhosttyWasmUrl")
            .replace(originalResize, optimizedResize)
            .replace(originalRender, scaledRender)
            .replace(originalSelectionCell, wideSelectionCell),
        map: null,
      };
    },
  };
}
