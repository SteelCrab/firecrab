import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import { ghosttyOptimizations } from "./build/ghosttyOptimizations.ts";

// Build the isolated harness with the same production optimizations as the app.
export default defineConfig({
  plugins: process.env.FIRECRAB_TERMINAL_BENCH_UNOPTIMIZED === "1" ? [] : [ghosttyOptimizations()],
  build: {
    outDir: "dist-benchmark",
    rolldownOptions: {
      input: fileURLToPath(new URL("./bench/index.html", import.meta.url)),
    },
  },
});
