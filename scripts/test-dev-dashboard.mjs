import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import http from "node:http";
import { createRequire } from "node:module";
import { mkdtemp, mkdir, writeFile, symlink, rm } from "node:fs/promises";
import net from "node:net";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
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
        assert.deepEqual(await response.json(), { source: root, session, dependencies, port, api_port: 5523, resource_port: 5524, isolated: false });
        ready = true; break;
      }
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  assert.ok(ready, `Vite did not answer the ownership endpoint: ${logs}`);
  const page = await fetch(url);
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /name="firecrab-resource-endpoint" content="\/__firecrab_resources\/api\/service\/resources"/);
  assert.ok(!html.includes(session), "public bootstrap must not expose the ownership credential");
  assert.equal(page.headers.get("content-security-policy"), null);
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

test("isolated dashboards reject missing private backend ports before listening", () => {
  const repository = fileURLToPath(new URL("..", import.meta.url));
  for (const [api, resource] of [["", "45122"], ["45121", ""], ["5523x", "45122"]]) {
    const child = spawnSync(process.execPath, [join(repository, "firecrab-cli/src/micromanager/dev/dashboard.mjs"), repository, "8098"], {
      env: {
        ...process.env, FIRECRAB_DEV_SESSION: randomUUID(), FIRECRAB_DEV_ISOLATED: "1",
        FIRECRAB_DEV_API_PORT: api, FIRECRAB_DEV_RESOURCE_PORT: resource,
      }, encoding: "utf8", timeout: 10000,
    });
    assert.notEqual(child.status, 0);
    assert.match(child.stderr, /Invalid development dashboard\/backend port/);
  }
});

test("isolated dashboards keep private API/resource routes and stop independently", { timeout: 180000 }, async (t) => {
  const repository = fileURLToPath(new URL("..", import.meta.url));
  const temporary = await mkdtemp(join(tmpdir(), "firecrab-isolated-dashboards-"));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const backend = async (identity) => {
    const listener = http.createServer((request, response) => {
      const { host, origin } = request.headers;
      const allowed = !identity.endsWith("-api") || !origin || origin === `http://${host}`;
      response.writeHead(allowed ? 200 : 403, { "Content-Type": "application/json", "Connection": "close" });
      response.end(JSON.stringify({ identity, host, origin, path: request.url }));
    });
    await new Promise(resolve => listener.listen(0, "127.0.0.1", resolve));
    t.after(() => new Promise(resolve => listener.close(resolve)));
    return listener.address().port;
  };
  const launch = async (name) => {
    const root = join(temporary, name);
    const frontend = join(root, "firecrab-frontend");
    await mkdir(frontend, { recursive: true });
    await writeFile(join(frontend, "package.json"), '{"type":"module"}');
    await writeFile(join(frontend, "index.html"), "<html><head><title>Legacy checkout</title></head><body>fixture</body></html>");
    await symlink(join(repository, "firecrab-frontend/node_modules"), join(frontend, "node_modules"), "junction");
    // An older source checkout still points to the primary API and permits any
    // Host. The embedded launcher must override it for a private instance.
    await writeFile(join(frontend, "vite.config.js"), `export default {server:{allowedHosts:true,proxy:{'/api':'http://127.0.0.1:5523','/ws':{target:'ws://127.0.0.1:5523',ws:true}}}};`);
    const api_port = await backend(`${name}-api`);
    const resource_port = await backend(`${name}-resources`);
    const reservation = net.createServer();
    await new Promise(resolve => reservation.listen(0, "127.0.0.1", resolve));
    const port = reservation.address().port;
    await new Promise(resolve => reservation.close(resolve));
    const session = randomUUID();
    const dependencies = randomUUID();
    const child = spawn(process.execPath, [join(repository, "firecrab-cli/src/micromanager/dev/dashboard.mjs"), root, String(port)], {
      cwd: tmpdir(), env: {
        ...process.env, FIRECRAB_DEV_SESSION: session, FIRECRAB_DEV_DEPENDENCIES: dependencies,
        FIRECRAB_DEV_API_PORT: String(api_port), FIRECRAB_DEV_RESOURCE_PORT: String(resource_port), FIRECRAB_DEV_ISOLATED: "1",
      }, stdio: ["ignore", "pipe", "pipe"],
    });
    const exited = new Promise(resolve => child.once("exit", resolve));
    let logs = "";
    child.stdout.on("data", value => logs += value);
    child.stderr.on("data", value => logs += value);
    t.after(() => { if (child.exitCode === null) child.kill(); });
    const url = `http://127.0.0.1:${port}`;
    const headers = { "X-Firecrab-Dev-Session": session };
    let ready = false;
    for (let attempt = 0; attempt < 300; attempt++) {
      if (child.exitCode !== null) throw new Error(logs);
      try {
        const response = await fetch(`${url}/__firecrab_dev/`, { headers });
        if (response.ok) {
          assert.deepEqual(await response.json(), { source: root, session, dependencies, port, api_port, resource_port, isolated: true });
          ready = true; break;
        }
      } catch {}
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert.ok(ready, logs);
    const stop = async () => {
      assert.equal((await fetch(`${url}/__firecrab_dev/stop`, { method: "POST", headers })).status, 202);
      assert.equal(await exited, 0, logs);
    };
    return { name, url, session, resource_port, stop };
  };
  const first = await launch("first");
  const second = await launch("second");
  for (const instance of [first, second]) {
    const headers = { Origin: instance.url };
    const api = await (await fetch(`${instance.url}/api/host`, { headers })).json();
    assert.equal(api.identity, `${instance.name}-api`);
    assert.equal(api.host, instance.url.slice("http://".length));
    assert.equal(api.origin, instance.url);
    assert.equal((await fetch(`${instance.url}/api/host`, { headers: { Origin: "https://evil.example" } })).status, 403);
    const resources = await (await fetch(`${instance.url}/__firecrab_resources/api/service/resources`, { headers })).json();
    assert.equal(resources.identity, `${instance.name}-resources`);
    assert.equal(resources.host, `127.0.0.1:${instance.resource_port}`);
    assert.equal(resources.origin, instance.url);
    assert.equal(resources.path, "/api/service/resources");
    const page = await fetch(instance.url);
    const html = await page.text();
    assert.match(html, /name="firecrab-resource-endpoint"/);
    assert.ok(!html.includes(instance.session));
    const policy = page.headers.get("content-security-policy");
    assert.equal(policy, `connect-src 'self' ws://localhost:${new URL(instance.url).port} ws://127.0.0.1:${new URL(instance.url).port}`);
    assert.ok(!policy.includes(":5524"), "legacy resource requests must not reach the primary controller");
    const badHost = await new Promise((resolve, reject) => {
      const request = http.request(`${instance.url}/api/host`, { headers: { Host: "evil.example", Origin: "http://evil.example" } }, response => {
        response.resume(); resolve(response.statusCode);
      });
      request.on("error", reject); request.end();
    });
    assert.equal(badHost, 403);
  }
  await first.stop();
  assert.equal((await (await fetch(`${second.url}/api/host`)).json()).identity, "second-api");
  assert.equal((await fetch(`${second.url}/__firecrab_resources/api/service/resources`)).status, 200);
  await second.stop();
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
