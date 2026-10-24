import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { test } from "node:test";

const frontend = new URL("../../firecrab-frontend/", import.meta.url);
const assets = new URL("dist/assets/", frontend);

test("the production console ships native WASM without xterm or inline Base64", async () => {
  const lock = JSON.parse(await readFile(new URL("package-lock.json", frontend), "utf8"));
  assert(!Object.keys(lock.packages).some(name => name.includes("@xterm/")), "xterm remains installed");
  const files = await readdir(assets);
  assert(!files.some(name => /xterm/i.test(name)), "xterm assets remain in the build");
  const wasm = files.filter(name => name.endsWith(".wasm"));
  assert.equal(wasm.length, 2, "the app and terminal must each emit one WASM asset");
  const terminal = wasm.find(name => name.startsWith("ghostty-vt-"));
  assert(terminal, "the external terminal WASM is missing");
  const binary = await readFile(new URL(terminal, assets));
  assert.deepEqual(binary, await readFile(new URL("node_modules/ghostty-web/ghostty-vt.wasm", frontend)));
  const exports = WebAssembly.Module.exports(new WebAssembly.Module(binary)).map(item => item.name);
  assert(exports.includes("ghostty_terminal_write"), "the WASM VT parser is missing");
  assert(exports.includes("ghostty_terminal_resize"), "WASM resize support is missing");
  for (const name of files.filter(name => name.endsWith(".js"))) {
    const source = await readFile(new URL(name, assets), "utf8");
    assert(!source.includes("data:application/wasm;base64,"), `inline WASM remains in ${name}`);
  }
});
