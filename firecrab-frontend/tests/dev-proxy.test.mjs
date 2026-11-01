import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer as createHttpServer } from "node:http";
import { createServer as createViteServer } from "vite";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const root = fileURLToPath(new URL("..", import.meta.url));
async function listen(server) {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return server.address().port;
}
async function close(server) {
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
}

test("development proxy requires an explicit API and uses the selected resource controller", async () => {
  const originalApi = process.env.FIRECRAB_DEV_API_PORT;
  const originalResource = process.env.FIRECRAB_DEV_RESOURCE_PORT;
  delete process.env.FIRECRAB_DEV_API_PORT;
  delete process.env.FIRECRAB_DEV_RESOURCE_PORT;
  const api = createHttpServer((request, response) => {
    response.setHeader("Content-Type", "application/json");
    response.end(
      JSON.stringify({ path: request.url, host: request.headers.host }),
    );
  });
  const resources = createHttpServer((request, response) => {
    response.setHeader("Content-Type", "application/json");
    response.end(
      JSON.stringify({ path: request.url, controllerId: "private-controller" }),
    );
  });
  resources.on("upgrade", (request, socket) => {
    const accept = createHash("sha1")
      .update(
        request.headers["sec-websocket-key"] +
          "258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
      )
      .digest("base64");
    socket.write(
      `HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    const data = Buffer.from(
      JSON.stringify({ path: request.url, host: request.headers.host }),
    );
    const header = Buffer.from([
      0x81,
      126,
      data.length >> 8,
      data.length & 255,
    ]);
    socket.write(Buffer.concat([header, data]));
    socket.on("data", (data) => {
      if ((data[0] & 15) === 8) socket.end(Buffer.from([0x88, 0]));
    });
  });
  let vite;
  try {
    await assert.rejects(
      createViteServer({
        root,
        configFile: `${root}/vite.config.ts`,
        logLevel: "silent",
      }),
      /Set FIRECRAB_DEV_API_PORT/,
    );
    process.env.FIRECRAB_DEV_API_PORT = String(await listen(api));
    process.env.FIRECRAB_DEV_RESOURCE_PORT = String(await listen(resources));
    vite = await createViteServer({
      root,
      configFile: `${root}/vite.config.ts`,
      logLevel: "silent",
      server: { host: "127.0.0.1", port: 0 },
    });
    await vite.listen();
    const origin = `http://127.0.0.1:${vite.httpServer.address().port}`;
    const document = await (await fetch(origin)).text();
    assert.match(
      document,
      /name="firecrab-resource-endpoint" content="\/__firecrab_resources\/api\/service\/resources"/,
    );
    const proxied = await (await fetch(`${origin}/api/host`)).json();
    assert.equal(proxied.path, "/api/host");
    assert.equal(proxied.host, new URL(origin).host);
    const controller = await (
      await fetch(`${origin}/__firecrab_resources/api/service/resources`)
    ).json();
    assert.deepEqual(controller, {
      path: "/api/service/resources",
      controllerId: "private-controller",
    });
    const path =
      "/api/service/agents/microvms/vm/task/terminal?controller=private-controller&cols=80&rows=24";
    const socket = new WebSocket(
      origin.replace("http:", "ws:") + "/__firecrab_resources" + path,
    );
    try {
      const received = await new Promise((resolve, reject) => {
        socket.onmessage = (event) => resolve(JSON.parse(event.data));
        socket.onerror = reject;
      });
      assert.deepEqual(received, {
        path,
        host: `127.0.0.1:${process.env.FIRECRAB_DEV_RESOURCE_PORT}`,
      });
    } finally {
      await new Promise((resolve) => {
        socket.onclose = resolve;
        socket.close();
      });
    }
  } finally {
    if (vite) await vite.close();
    if (api.listening) await close(api);
    if (resources.listening) await close(resources);
    if (originalApi === undefined) delete process.env.FIRECRAB_DEV_API_PORT;
    else process.env.FIRECRAB_DEV_API_PORT = originalApi;
    if (originalResource === undefined)
      delete process.env.FIRECRAB_DEV_RESOURCE_PORT;
    else process.env.FIRECRAB_DEV_RESOURCE_PORT = originalResource;
  }
});
