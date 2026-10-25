import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import net from "node:net";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

test("embedded dashboard starts real Vite and authenticates shutdown", { timeout: 180000 }, async (t) => {
  const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
  const reservation = net.createServer();
  await new Promise(resolve => reservation.listen(0, "127.0.0.1", resolve));
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  const session = randomUUID();
  const dependencies = randomUUID();
  const child = spawn(process.execPath, [resolve(root, "firecrab-cli/src/micromanager/dev/dashboard.mjs"), root, String(port)], {
    cwd: tmpdir(), env: { ...process.env, FIRECRAB_DEV_SESSION: session, FIRECRAB_DEV_DEPENDENCIES: dependencies }, stdio: ["ignore", "pipe", "pipe"],
  });
  const exited = new Promise(resolve => child.once("exit", (code) => resolve(code)));
  let logs = "";
  child.stdout.on("data", value => logs += value);
  child.stderr.on("data", value => logs += value);
  t.after(() => { if (child.exitCode === null) child.kill(); });
  const url = `http://127.0.0.1:${port}`;
  const headers = { "X-Firecrab-Dev-Session": session };
  let ready = false;
  for (let attempt = 0; attempt < 1500; attempt++) {
    if (child.exitCode !== null) throw new Error(logs);
    try {
      const response = await fetch(`${url}/__firecrab_dev/`, { headers });
      if (response.headers.get("content-type")?.includes("application/json")) {
        assert.deepEqual(await response.json(), { source: root, session, dependencies });
        ready = true; break;
      }
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.ok(ready, `Vite did not answer the ownership endpoint: ${logs}`);
  assert.equal((await fetch(url)).status, 200);
  assert.equal((await fetch(`${url}/@vite/client`)).status, 200);
  const terminal = await fetch(`${url}/src/lib/wasmTerminal.ts`);
  assert.equal(terminal.status, 200, await terminal.text());
  const wasm = await fetch(`${url}/node_modules/ghostty-web/dist/ghostty-vt.wasm`);
  assert.equal(wasm.status, 200);
  assert.equal(WebAssembly.validate(await wasm.arrayBuffer()), true);
  assert.equal((await fetch(`${url}/__firecrab_dev/stop`, { method: "POST" })).status, 403);
  assert.equal((await fetch(`${url}/__firecrab_dev/stop`, { method: "POST", headers: { "X-Firecrab-Dev-Session": randomUUID() } })).status, 403);
  assert.equal((await fetch(`${url}/__firecrab_dev/stop`, { method: "POST", headers })).status, 202);
  assert.equal(await exited, 0, logs);
});
