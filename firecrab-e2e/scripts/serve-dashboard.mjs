import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
const source = fileURLToPath(
  new URL("../../firecrab-frontend/", import.meta.url),
);
const vite = fileURLToPath(
  new URL(
    "../../firecrab-frontend/node_modules/vite/bin/vite.js",
    import.meta.url,
  ),
);
const address = new URL(
  process.env.FIRECRAB_E2E_PREVIEW_URL ||
    process.env.FIRECRAB_MOCKED_E2E_BASE_URL ||
    "http://127.0.0.1:8097",
);
if (
  !["127.0.0.1", "localhost", "[::1]"].includes(address.hostname) ||
  !address.port
)
  throw Error("Mock dashboard must use an explicit loopback port");
const directory = await mkdtemp(join(tmpdir(), "firecrab-react-e2e-"));
let child,
  stopping = false;
const stop = () => {
  stopping = true;
  child?.kill("SIGTERM");
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
const run = (args) =>
  new Promise((resolve, reject) => {
    child = spawn(process.execPath, [vite, ...args], {
      cwd: source,
      stdio: "inherit",
      env: process.env,
    });
    child.once("error", reject);
    child.once("exit", (code) => {
      child = null;
      resolve(code);
    });
  });
try {
  const built = await run(["build", "--outDir", directory, "--emptyOutDir"]);
  if (!stopping && built !== 0) throw Error("React dashboard build failed");
  if (!stopping) {
    const exitCode = await run([
      "preview",
      "--outDir",
      directory,
      "--host",
      address.hostname,
      "--port",
      address.port,
      "--strictPort",
    ]);
    process.exitCode = stopping ? 0 : (exitCode ?? 1);
  }
} catch (error) {
  if (!stopping) {
    console.error(error);
    process.exitCode = 1;
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}
