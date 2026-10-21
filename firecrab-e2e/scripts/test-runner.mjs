import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const require = createRequire(import.meta.url);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function run(script, args = [], flags = {}) {
  const env = { ...process.env };
  delete env.FIRECRAB_E2E_SKIP_GUEST_BOOT;
  delete env.FIRECRAB_E2E_REQUIRE_GUEST_BOOT;
  delete env.FIRECRAB_E2E_REQUIRE_RUNNING_API;
  return spawnSync(process.execPath, [script, ...args], {
    cwd: root,
    env: { ...env, ...flags },
    encoding: "utf8",
    timeout: 30_000,
  });
}

test("import-only launcher works without shell environment syntax and forwards arguments", () => {
  const result = run("scripts/test-import.mjs", ["--list", "tests/network-ipv6.spec.ts"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Total: 2 tests in 1 file/);
});

test("a required guest-boot run rejects import-only configuration", () => {
  const result = run(require.resolve("@playwright/test/cli"), ["test", "--list"], {
    FIRECRAB_E2E_REQUIRE_GUEST_BOOT: "1",
    FIRECRAB_E2E_SKIP_GUEST_BOOT: "1",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stdout + result.stderr, /Guest boot is required/);
});

test("managed runtime QA cannot silently launch a replacement API", () => {
  const result = run("scripts/ensure-api.mjs", [], { FIRECRAB_E2E_REQUIRE_RUNNING_API: "1" });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /refusing to start a second API/);
});
