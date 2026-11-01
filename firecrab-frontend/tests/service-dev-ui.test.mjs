import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import net from "node:net";

test(
  "service dev serves React and proxies to the selected private API",
  { timeout: 20000 },
  async (t) => {
    const checkout = resolve(fileURLToPath(new URL("../..", import.meta.url)));
    const backend = createServer((request, response) => {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(
        JSON.stringify({
          identity: "private-api",
          path: request.url,
          host: request.headers.host,
        }),
      );
    });
    await new Promise((resolve) => backend.listen(0, "127.0.0.1", resolve));
    t.after(() => new Promise((resolve) => backend.close(resolve)));
    const apiPort = backend.address().port;
    const reserve = net.createServer();
    await new Promise((resolve) => reserve.listen(0, "127.0.0.1", resolve));
    const port = reserve.address().port;
    await new Promise((resolve) => reserve.close(resolve));
    const session = randomUUID();
    const child = spawn(
      process.execPath,
      [
        resolve(checkout, "firecrab-cli/src/micromanager/dev/dashboard.mjs"),
        checkout,
        String(port),
      ],
      {
        env: {
          ...process.env,
          FIRECRAB_DEV_SESSION: session,
          FIRECRAB_DEV_DEPENDENCIES: "fixture",
          FIRECRAB_DEV_ISOLATED: "1",
          FIRECRAB_DEV_API_PORT: String(apiPort),
          FIRECRAB_DEV_RESOURCE_PORT: String(apiPort),
        },
      },
    );
    let logs = "";
    child.stdout.on("data", (v) => (logs += v));
    child.stderr.on("data", (v) => (logs += v));
    t.after(() => {
      if (child.exitCode === null) child.kill();
    });
    const url = `http://127.0.0.1:${port}`;
    let ready = false;
    for (let i = 0; i < 100; i++) {
      if (child.exitCode !== null) throw Error(logs);
      try {
        const response = await fetch(`${url}/__firecrab_dev/`, {
          headers: { "X-Firecrab-Dev-Session": session },
        });
        if (response.ok) {
          ready = true;
          break;
        }
      } catch {}
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(ready, logs);
    const html = await (await fetch(url)).text();
    assert.match(html, /src\/main\.tsx/);
    assert.match(html, /firecrab-resource-endpoint/);
    // Reoptimizing dependencies in a second Vite process must not invalidate
    // modules that the running service dev dashboard already served.
    const source = await (await fetch(`${url}/src/main.tsx`)).text();
    const dependency = source.match(
      /from "(\/node_modules\/\.firecrab-dev-vite\/[^" ]+react\.js\?[^" ]+)"/,
    );
    assert.ok(
      dependency,
      "service dev must use its own optimized dependency cache",
    );
    const reserveOther = net.createServer();
    await new Promise((resolve) =>
      reserveOther.listen(0, "127.0.0.1", resolve),
    );
    const otherPort = reserveOther.address().port;
    await new Promise((resolve) => reserveOther.close(resolve));
    const vite = spawn(
      process.execPath,
      [
        resolve(checkout, "firecrab-frontend/node_modules/vite/bin/vite.js"),
        "--force",
        "--host",
        "127.0.0.1",
        "--port",
        String(otherPort),
        "--strictPort",
      ],
      {
        cwd: resolve(checkout, "firecrab-frontend"),
        env: { ...process.env, FIRECRAB_DEV_API_PORT: String(apiPort) },
      },
    );
    t.after(() => {
      if (vite.exitCode === null) vite.kill();
    });
    let otherReady = false;
    for (let i = 0; i < 100; i++) {
      if (vite.exitCode !== null) throw Error("second Vite server exited");
      try {
        if ((await fetch(`http://127.0.0.1:${otherPort}/`)).ok) {
          otherReady = true;
          break;
        }
      } catch {}
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(otherReady);
    assert.equal(
      (await fetch(`${url}${dependency[1]}`)).status,
      200,
      "another Vite server invalidated the running dashboard",
    );
    const response = await fetch(`${url}/api/example`, {
      headers: { Origin: url },
    });
    const json = await response.json();
    assert.equal(json.identity, "private-api");
    assert.equal(json.host, `127.0.0.1:${port}`);
    const stop = await fetch(`${url}/__firecrab_dev/stop`, {
      method: "POST",
      headers: { "X-Firecrab-Dev-Session": session },
    });
    assert.equal(stop.status, 202);
  },
);
