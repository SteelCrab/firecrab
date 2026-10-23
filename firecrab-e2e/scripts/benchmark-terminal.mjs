import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { cpus, platform, release, arch, loadavg } from "node:os";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { chromium } from "@playwright/test";

const repo = fileURLToPath(new URL("../../", import.meta.url));
const frontend = path.join(repo, "firecrab-frontend");
const root = path.join(frontend, "dist-benchmark");
const output = path.resolve(process.env.FIRECRAB_TERMINAL_BENCH_OUTPUT ?? path.join(repo, "benchmarks/terminal/latest"));
const samples = Number(process.env.FIRECRAB_TERMINAL_BENCH_SAMPLES ?? 7);
const profileOnly = process.argv.includes("--profile-resize");
assert(Number.isInteger(samples) && samples >= 3, "Use at least three measured samples");
const warmups = 2;
const workloads = ["plain", "ansi", "unicode", "wrap", "tui"];
const grids = [[80, 24], [160, 48]];
const engines = ["xterm", "ghostty"];
const result = { metadata: {}, coldStart: [], output: [], input: [], resize: [], pacedResize: [], memory: [], assets: [] };
const resizeProfiles = [];
const hash = value => createHash("sha256").update(value).digest("hex");
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * p) - 1)];
const median = values => percentile(values, 0.5);

execFileSync("npm", ["run", "benchmark:build"], { cwd: frontend, stdio: "inherit" });
await mkdir(output, { recursive: true });
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" };
const server = createServer(async (request, response) => {
  const filename = path.resolve(root, `.${new URL(request.url, "http://localhost").pathname}`);
  if (!filename.startsWith(`${root}${path.sep}`)) { response.writeHead(403).end(); return; }
  try {
    const content = await readFile(filename);
    response.writeHead(200, { "Content-Type": types[path.extname(filename)] ?? "application/octet-stream", "Content-Length": content.length });
    response.end(content);
  } catch { response.writeHead(404).end(); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const url = `http://127.0.0.1:${server.address().port}/bench/index.html`;
const browser = await chromium.launch({ headless: true, args: ["--enable-precise-memory-info"] });
let invalid = false;

try {
  const lock = JSON.parse(await readFile(path.join(frontend, "package-lock.json"), "utf8"));
  result.metadata = {
    recordedAt: new Date().toISOString(), platform: platform(), release: release(), architecture: arch(),
    cpu: cpus()[0]?.model, logicalCpus: cpus().length, browser: browser.version(), node: process.version,
    revision: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim(),
    dirty: execFileSync("git", ["status", "--porcelain"], { cwd: repo, encoding: "utf8" }).trim().length > 0,
    diffSha256: hash(execFileSync("git", ["diff", "--binary"], { cwd: repo })),
    packages: Object.fromEntries(["ghostty-web", "@xterm/xterm", "@xterm/addon-fit"].map(name => [name, lock.packages[`node_modules/${name}`].version])),
    samples, warmups, grids, chunkSize: 4093, viewport: { width: 1600, height: 1100 }, loadAverageBefore: loadavg(),
    optimization: process.env.FIRECRAB_TERMINAL_BENCH_UNOPTIMIZED === "1" ? "unoptimized" : "optimized",
    method: "Production build; local HTTP; installed system font fallback; one active page; alternating engine order. Output waits for the final marker in the parsed buffer and one following animation frame; write callbacks are not used for timing. Input is synthetic keydown with local echo and two frames; neither metric measures GPU presentation. Heap statistics are Chromium CDP observations, not total process memory. Fresh contexts do not imply a cold browser process or cold OS cache.",
  };
  result.metadata.sourceSha256 = Object.fromEntries(await Promise.all([
    "firecrab-frontend/src/lib/terminalEngine.ts", "firecrab-frontend/src/lib/terminal.ts",
    "firecrab-frontend/bench/terminal.ts", "firecrab-frontend/bench/workloads.ts",
    "firecrab-frontend/package-lock.json", "firecrab-frontend/vite.benchmark.config.ts",
    "firecrab-frontend/build/ghosttyOptimizations.ts",
    "firecrab-e2e/scripts/benchmark-terminal.mjs",
  ].map(async filename => [filename, hash(await readFile(path.join(repo, filename)))])));
  const manifest = await readFile(path.join(root, "bench/index.html"), "utf8");
  assert(manifest.includes("assets/"), "Benchmark must use a production bundle");
  if (profileOnly) {
    for (const deviceScaleFactor of [1, 2]) {
      const context = await browser.newContext({ viewport: result.metadata.viewport, deviceScaleFactor });
      const page = await context.newPage();
      await page.goto(url);
      await page.waitForFunction(() => Boolean(window.terminalBenchmark));
      await page.evaluate(() => window.terminalBenchmark.mount("ghostty", 80, 24));
      resizeProfiles.push(await page.evaluate(() => window.terminalBenchmark.profileResize()));
      await context.close();
    }
  } else {
  for (let index = 0; index < samples; index++) {
    for (const engine of index % 2 ? [...engines].reverse() : engines) {
      const context = await browser.newContext({ viewport: result.metadata.viewport });
      const page = await context.newPage();
      await page.goto(url);
      await page.waitForFunction(() => Boolean(window.terminalBenchmark));
      const ready = await page.evaluate(engine => window.terminalBenchmark.mount(engine, 80, 24), engine);
      const resources = await page.evaluate(() => performance.getEntriesByType("resource").filter(item => item.name.startsWith(location.origin)).map(item => ({ name: new URL(item.name).pathname, bytes: item.encodedBodySize })));
      result.coldStart.push({ engine, sample: index, ...ready, resourceBytes: resources.reduce((sum, item) => sum + item.bytes, 0) });
      if (index === 0) {
        for (const resource of resources) {
          const bytes = await readFile(path.join(root, resource.name));
          if (engine === "ghostty" && result.metadata.optimization === "optimized" && resource.name.includes("ghostty-web") && resource.name.endsWith(".js")) {
            assert(!bytes.includes(Buffer.from("data:application/wasm;base64,")), "Optimized Ghostty JS must not embed WASM");
          }
          result.assets.push({ engine, ...resource, gzipBytes: gzipSync(bytes).length });
        }
      }
      await context.close();
    }
  }
  if (result.metadata.optimization === "optimized") assert(result.assets.some(asset => asset.engine === "ghostty" && asset.name.endsWith(".wasm")), "Optimized Ghostty must load an external WASM asset");

  const context = await browser.newContext({ viewport: result.metadata.viewport });
  const page = await context.newPage();
  const pageErrors = [];
  page.on("pageerror", error => pageErrors.push(error.message));
  await page.goto(url);
  await page.waitForFunction(() => Boolean(window.terminalBenchmark));
  const cdp = await context.newCDPSession(page);
  for (let sample = -warmups; sample < samples; sample++) {
    for (const [cols, rows] of grids) {
      for (const workload of workloads) {
        for (const engine of sample % 2 ? [...engines].reverse() : engines) {
          await page.evaluate(({ engine, cols, rows }) => window.terminalBenchmark.mount(engine, cols, rows), { engine, cols, rows });
          const measured = await page.evaluate(workload => window.terminalBenchmark.output(workload), workload);
          if (!measured.correct) invalid = true;
          if (sample >= 0) {
            const { actual, expected, ...timing } = measured;
            result.output.push({ engine, sample, cols, rows, workload, ...timing, screenSha256: hash(actual.text), ...(measured.correct ? {} : { actual, expected }) });
          }
          if (sample === 0 && cols === 80 && workload === "unicode") await page.screenshot({ path: path.join(output, `${engine}.png`) });
        }
      }
    }
    for (const engine of sample % 2 ? [...engines].reverse() : engines) {
      await page.evaluate(engine => window.terminalBenchmark.mount(engine, 80, 24), engine);
      const latency = await page.evaluate(() => window.terminalBenchmark.input());
      const resizeMs = await page.evaluate(() => window.terminalBenchmark.resize());
      const paced = await page.evaluate(() => window.terminalBenchmark.resizePaced());
      if (sample >= 0) {
        result.input.push({ engine, sample, milliseconds: latency, p50Ms: median(latency), p95Ms: percentile(latency, 0.95) });
        result.resize.push({ engine, sample, elapsedMs: resizeMs, operations: 40 });
        result.pacedResize.push({ engine, sample, ...paced, operations: 40 });
      }
    }
    console.log(`Terminal benchmark: ${sample < 0 ? `warmup ${sample + warmups + 1}` : `sample ${sample + 1}/${samples}`}`);
  }

  for (const engine of engines) {
    await page.evaluate(() => window.terminalBenchmark.dispose());
    await cdp.send("HeapProfiler.collectGarbage");
    const before = await cdp.send("Runtime.getHeapUsage");
    for (let index = 0; index < 10; index++) {
      await page.evaluate(engine => window.terminalBenchmark.mount(engine, 80, 24), engine);
      await page.evaluate(() => window.terminalBenchmark.output("plain"));
      await page.evaluate(() => window.terminalBenchmark.dispose());
    }
    await cdp.send("HeapProfiler.collectGarbage");
    result.memory.push({ engine, cycles: 10, before, after: await cdp.send("Runtime.getHeapUsage") });
  }
  await context.close();
  result.metadata.loadAverageAfter = loadavg();
  assert.deepEqual(pageErrors, [], "Browser errors invalidate the benchmark");
  }
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}

if (profileOnly) {
  await writeFile(path.join(output, "resize-profile.json"), `${JSON.stringify({ metadata: result.metadata, resizeProfiles }, null, 2)}\n`);
  console.log(JSON.stringify(resizeProfiles, null, 2));
} else {
await writeFile(path.join(output, "results.json"), `${JSON.stringify(result, null, 2)}\n`);
await writeFile(path.join(output, "output.csv"), ["engine,sample,cols,rows,workload,bytes,elapsedMs,mibPerSecond,correct", ...result.output.map(row => [row.engine, row.sample, row.cols, row.rows, row.workload, row.bytes, row.elapsedMs, row.mibPerSecond, row.correct].join(","))].join("\n") + "\n");
const lines = [
  "# Firecrab terminal benchmark", "", `Recorded: ${result.metadata.recordedAt}`, "",
  `${result.metadata.cpu} / ${result.metadata.platform} ${result.metadata.architecture} / Chromium ${result.metadata.browser}`, "",
  `Packages: ${JSON.stringify(result.metadata.packages)}`, "",
  `${samples} measured samples, ${warmups} warmups, 4093-byte chunks, 5000-line scrollback.`, "",
  result.metadata.method, "",
  "## Output to frame opportunity", "",
  "| Grid | Workload | xterm median ms | Ghostty median ms | xterm MiB/s | Ghostty MiB/s | Correct samples |",
  "|---|---|---:|---:|---:|---:|---|",
];
for (const [cols, rows] of grids) for (const workload of workloads) {
  const grouped = engines.map(engine => result.output.filter(row => row.engine === engine && row.cols === cols && row.rows === rows && row.workload === workload));
  lines.push(`| ${cols}×${rows} | ${workload} | ${grouped.map(group => median(group.map(row => row.elapsedMs)).toFixed(2)).join(" | ")} | ${grouped.map(group => median(group.map(row => row.mibPerSecond)).toFixed(2)).join(" | ")} | ${grouped.map(group => `${group.filter(row => row.correct).length}/${group.length}`).join("; ")} |`);
}
lines.push("", "## Startup, local echo and resize", "", "| Engine | Fresh-context ready median ms | Input p50 ms | Input p95 ms | 40 resizes median ms |", "|---|---:|---:|---:|---:|");
for (const engine of engines) {
  const latency = result.input.filter(row => row.engine === engine).flatMap(row => row.milliseconds);
  lines.push(`| ${engine} | ${median(result.coldStart.filter(row => row.engine === engine).map(row => row.readyMs)).toFixed(2)} | ${median(latency).toFixed(2)} | ${percentile(latency, 0.95).toFixed(2)} | ${median(result.resize.filter(row => row.engine === engine).map(row => row.elapsedMs)).toFixed(2)} |`);
}
lines.push("", "## Bundle payload", "", "| Engine | Loaded resources bytes | Gzip bytes (computed, not transferred) |", "|---|---:|---:|");
for (const engine of engines) {
  const assets = result.assets.filter(item => item.engine === engine);
  lines.push(`| ${engine} | ${assets.reduce((sum, item) => sum + item.bytes, 0)} | ${assets.reduce((sum, item) => sum + item.gzipBytes, 0)} |`);
}
lines.push("", "## Paced resize (one grid change per animation frame)", "", "| Engine | 40 frames median ms | Handler sum median ms | Handler p95 ms |", "|---|---:|---:|---:|");
for (const engine of engines) {
  const rows = result.pacedResize.filter(row => row.engine === engine);
  lines.push(`| ${engine} | ${median(rows.map(row => row.elapsedMs)).toFixed(2)} | ${median(rows.map(row => row.handlers.reduce((sum, value) => sum + value, 0))).toFixed(2)} | ${percentile(rows.flatMap(row => row.handlers), 0.95).toFixed(2)} |`);
}
lines.push("", "## Limits", "", "- Loopback headless Chromium only. No guest VM, network latency, real OS IME, or GPU presentation timing is measured.", "- A two-frame floor affects small workloads and synthetic input. Throughput includes scheduling and rendering opportunities, not just parsing.", "- Memory captures JS heap and backing-storage observations before/after ten mount/dispose cycles, not a peak-memory or leak proof.", "- Compare correctness before interpreting speed. Failed cases retain actual and expected screens in results.json.", "- Timing is informational. No CI performance threshold is imposed from one machine.", "", `Correctness: ${invalid ? "FAIL" : "PASS"}`, "");
await writeFile(path.join(output, "report.md"), lines.join("\n"));
console.log(`Report: ${path.join(output, "report.md")}`);
if (invalid) process.exitCode = 1;
}
