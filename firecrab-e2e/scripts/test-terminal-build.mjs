import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { test } from "node:test";

const frontend = new URL("../../firecrab-frontend/", import.meta.url);
const assets = new URL("dist/assets/", frontend);

test("the production dashboard ships the JavaScript terminal without WASM assets", async () => {
  const lock = JSON.parse(
    await readFile(new URL("package-lock.json", frontend), "utf8"),
  );
  assert(lock.packages["node_modules/@xterm/xterm"]);
  const files = await readdir(assets);
  assert(!files.some((name) => name.endsWith(".wasm")));
  for (const name of files.filter((name) => name.endsWith(".js"))) {
    const source = await readFile(new URL(name, assets), "utf8");
    assert(!source.includes("data:application/wasm;base64,"));
  }
});
