import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

function port(name: string): number | null {
  const raw = process.env[name];
  if (!raw) return null;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 1 || value > 65535)
    throw new Error(`Invalid ${name}`);
  return value;
}

export default defineConfig(({ command, isPreview }) => {
  const apiPort = port("FIRECRAB_DEV_API_PORT");
  const resourcePort = port("FIRECRAB_DEV_RESOURCE_PORT");
  if (command === "serve" && !isPreview && !apiPort)
    throw new Error(
      "Set FIRECRAB_DEV_API_PORT to the selected development API port",
    );
  const proxy = apiPort
    ? {
        "/api": { target: `http://127.0.0.1:${apiPort}`, changeOrigin: false },
        "/auth/callback": {
          target: `http://127.0.0.1:${apiPort}`,
          changeOrigin: false,
        },
        "/ws": {
          target: `ws://127.0.0.1:${apiPort}`,
          changeOrigin: false,
          ws: true,
        },
        ...(resourcePort
          ? {
              "/__firecrab_resources": {
                target: `http://127.0.0.1:${resourcePort}`,
                changeOrigin: true,
                ws: true,
                rewrite: (path: string) =>
                  path.slice("/__firecrab_resources".length) || "/",
              },
            }
          : {}),
      }
    : {};
  return {
    cacheDir: `node_modules/.vite-react/${apiPort ?? "build"}`,
    plugins: [
      react(),
      {
        name: "firecrab-resource-endpoint",
        transformIndexHtml: () =>
          resourcePort && !process.env.FIRECRAB_DEV_SESSION
            ? [
                {
                  tag: "meta",
                  attrs: {
                    name: "firecrab-resource-endpoint",
                    content: "/__firecrab_resources/api/service/resources",
                  },
                  injectTo: "head-prepend" as const,
                },
              ]
            : [],
      },
    ],
    server: { host: "127.0.0.1", port: 8080, strictPort: true, proxy },
    build: { target: "es2022", assetsInlineLimit: 0 },
  };
});
