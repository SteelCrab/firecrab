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
let server;
const control = (request, response) => {
  if (request.headers["x-firecrab-dev-session"] !== session) {
    response.writeHead(403).end();
    return;
  }
  if (request.method === "GET" && request.url === "/") {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ source: resolve(root, ".."), session }));
  } else if (request.method === "POST" && request.url === "/stop") {
    response.writeHead(202).end();
    setImmediate(async () => { await server.close(); process.exit(0); });
  } else {
    response.writeHead(404).end();
  }
};
server = await createServer({
  root,
  server: { host: "127.0.0.1", port, strictPort: true },
  plugins: [{ name: "firecrab-dev-control", configureServer(vite) {
    vite.middlewares.use("/__firecrab_dev", control);
  } }],
});
await server.listen();
server.printUrls();
