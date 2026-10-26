// Embedded in the CLI so any checkout with a Vite frontend can be selected.
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const root = resolve(process.argv[2], "firecrab-frontend");
const session = process.env.FIRECRAB_DEV_SESSION;
if (!session) throw new Error("Missing dashboard session");
const require = createRequire(resolve(root, "package.json"));
const { createServer } = await import(pathToFileURL(require.resolve("vite")).href);
const port = Number(process.argv[3]);
const isolated = process.env.FIRECRAB_DEV_ISOLATED === "1";
const api_port = Number(process.env.FIRECRAB_DEV_API_PORT || (isolated ? NaN : 5523));
const resource_port = Number(process.env.FIRECRAB_DEV_RESOURCE_PORT || (isolated ? NaN : 5524));
for (const value of [port, api_port, resource_port]) {
  if (!Number.isInteger(value) || value < 1 || value > 65535) throw new Error("Invalid development dashboard/backend port");
}
const resourcePrefix = "/__firecrab_resources";
const proxy = {
  "/api": { target: `http://127.0.0.1:${api_port}`, changeOrigin: false },
  "/auth/callback": { target: `http://127.0.0.1:${api_port}`, changeOrigin: false },
  "/ws": { target: `ws://127.0.0.1:${api_port}`, changeOrigin: false, ws: true },
  [resourcePrefix]: {
    target: `http://127.0.0.1:${resource_port}`, changeOrigin: true,
    rewrite: path => path.slice(resourcePrefix.length) || "/",
  },
};
// Older dashboards call the primary native resource port directly. Isolated
// instances permit only this dashboard's bridge and WebSocket proxy instead.
const csp = `connect-src 'self' ws://localhost:${port} ws://127.0.0.1:${port}`;
let server;
const control = (request, response) => {
  if (request.headers["x-firecrab-dev-session"] !== session) {
    response.writeHead(403).end();
    return;
  }
  if (request.method === "GET" && request.url === "/") {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ source: resolve(root, ".."), session, dependencies: process.env.FIRECRAB_DEV_DEPENDENCIES, port, api_port, resource_port, isolated }));
  } else if (request.method === "POST" && request.url === "/stop") {
    response.writeHead(202).end();
    setImmediate(async () => { await server.close(); process.exit(0); });
  } else {
    response.writeHead(404).end();
  }
};
server = await createServer({
  root,
  server: { host: "127.0.0.1", port, strictPort: true, proxy },
  plugins: [{
    name: "firecrab-dev-control",
    configResolved(config) {
      // Inline arrays can merge with an older checkout's allowedHosts:true.
      // Apply these invariants after all source configuration has resolved.
      config.server.host = "127.0.0.1";
      config.server.port = port;
      config.server.strictPort = true;
      config.server.allowedHosts = [];
      config.server.proxy = { ...config.server.proxy, ...proxy };
      if (isolated) config.server.headers = { ...config.server.headers, "Content-Security-Policy": csp };
    },
    configureServer(vite) {
      vite.middlewares.use("/__firecrab_dev", control);
    },
    transformIndexHtml() {
      return [{
        tag: "meta", attrs: { name: "firecrab-resource-endpoint", content: `${resourcePrefix}/api/service/resources` },
        injectTo: "head-prepend",
      }];
    },
  }],
});
await server.listen();
server.printUrls();
