#!/usr/bin/env node
// Set the import-only environment without depending on a POSIX npm shell.
import { spawn } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const child = spawn(
  process.execPath,
  [require.resolve("@playwright/test/cli"), "test", ...process.argv.slice(2)],
  {
    stdio: "inherit",
    env: { ...process.env, FIRECRAB_E2E_SKIP_GUEST_BOOT: "1" },
  },
);
child.on("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.on("exit", (code) => {
  process.exitCode = code ?? 1;
});
