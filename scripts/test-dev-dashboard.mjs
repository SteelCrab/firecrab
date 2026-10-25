import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import http from "node:http";
import { createRequire } from "node:module";
import net from "node:net";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
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
        assert.deepEqual(await response.json(), { source: root, session, dependencies, port });
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

test("development API proxy preserves the dashboard origin on a custom port", { timeout: 180000 }, async (t) => {
  const frontend = fileURLToPath(new URL("../firecrab-frontend/", import.meta.url));
  const require = createRequire(resolve(frontend, "package.json"));
  const { createServer, loadConfigFromFile } = await import(pathToFileURL(require.resolve("vite")).href);
  const { config } = await loadConfigFromFile({ command: "serve", mode: "test" }, resolve(frontend, "vite.config.js"));
  // Match the API's same-origin policy without a VM or a listener on port 5523.
  const upstream = http.createServer((request, response) => {
    const { host, origin } = request.headers;
    response.writeHead(origin === `http://${host}` ? 200 : 403, {
      "Content-Type": "application/json", "Connection": "close",
    });
    response.end(JSON.stringify({ host, origin, method: request.method }));
  });
  await new Promise(resolve => upstream.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise(resolve => upstream.close(resolve)));
  const target = `http://127.0.0.1:${upstream.address().port}`;
  const proxy = config.server.proxy["/api"];
  const server = await createServer({
    ...config, configFile: false, root: frontend,
    server: {
      ...config.server, host: "127.0.0.1", port: 0, strictPort: true,
      proxy: { ...config.server.proxy, "/api": typeof proxy === "string" ? target : { ...proxy, target } },
    },
  });
  t.after(() => server.close());
  await server.listen();
  const port = server.httpServer.address().port;
  const request = async (method, host, origin) => {
    const response = await new Promise((resolve, reject) => {
      const pending = http.request({
        hostname: "127.0.0.1", port, path: "/api/proxy-origin", method,
        headers: { Host: host, Origin: origin },
      }, resolve);
      pending.on("error", reject);
      pending.end();
    });
    let body = "";
    for await (const chunk of response) body += chunk;
    return { status: response.statusCode, body };
  };
  for (const hostname of ["localhost", "127.0.0.1"]) {
    const host = `${hostname}:${port}`;
    for (const method of ["GET", "POST"]) {
      const response = await request(method, host, `http://${host}`);
      assert.equal(response.status, 200, `${method} from http://${host}`);
      assert.deepEqual(JSON.parse(response.body), { host, origin: `http://${host}`, method });
    }
    for (const origin of ["https://evil.example", `http://${hostname}:${port + 1}`]) {
      assert.equal((await request("POST", host, origin)).status, 403, origin);
    }
  }
  const untrustedHost = `evil.example:${port}`;
  assert.equal((await request("POST", untrustedHost, `http://${untrustedHost}`)).status, 403);
});
