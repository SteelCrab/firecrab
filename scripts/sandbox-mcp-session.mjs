// Interactive official MCP SDK client. Each operation is selected by its operator/model.
// This file never chooses resources, provisions anything automatically, or reads credentials.
import { createInterface } from "node:readline";
import { appendFileSync, closeSync, openSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [binary, sdkRoot, api, trace, mode = "read-only"] = process.argv.slice(2);
if (!binary || !sdkRoot || !api || !trace || !["read-only", "mutations-authorized"].includes(mode)) {
  throw new Error("Usage: node sandbox-mcp-session.mjs /absolute/firecrab /absolute/node_modules/@modelcontextprotocol/sdk http://127.0.0.1:PORT /absolute/trace.jsonl [read-only|mutations-authorized]");
}
const { Client } = await import(pathToFileURL(join(resolve(sdkRoot), "dist/esm/client/index.js")));
const { StdioClientTransport } = await import(pathToFileURL(join(resolve(sdkRoot), "dist/esm/client/stdio.js")));
const endpoint = new URL(api);
if (!["127.0.0.1", "localhost", "[::1]"].includes(endpoint.hostname)
    || !["http:", "https:"].includes(endpoint.protocol) || endpoint.username || endpoint.password
    || endpoint.pathname !== "/" || endpoint.search || endpoint.hash) {
  throw new Error("Use the explicitly approved loopback API origin without credentials");
}
const output = openSync(resolve(trace), "ax", 0o600);
const record = value => appendFileSync(output, `${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`);
record({ binary: resolve(binary), binarySha256: createHash("sha256").update(readFileSync(resolve(binary))).digest("hex") });
const client = new Client({ name: "firecrab-414-interactive-agent", version: "1.0.0" }, { capabilities: {} });
const transport = new StdioClientTransport({ command: resolve(binary), args: ["--api", endpoint.origin, "mcp"], stderr: "pipe" });
// macOS canonical terminals truncate long JSON lines; readline owns framing here.
const rawTerminal = process.stdin.isTTY === true;
if (rawTerminal) process.stdin.setRawMode(true);
const lines = createInterface({ input: process.stdin, terminal: false });
try {
  await client.connect(transport);
  const { tools } = await client.listTools();
  record({ method: "tools/list", result: { tools }, mode, api: endpoint.origin });
  process.stdout.write(`${JSON.stringify({ ready: true, mode, api: endpoint.origin, tools: tools.map(t => ({ name: t.name, readOnly: t.annotations?.readOnlyHint === true })) })}\n`);
  for await (const line of lines) {
    if (!line.trim()) continue;
    if (line.trim() === "quit") break;
    let request;
    try {
      request = JSON.parse(line);
      const tool = tools.find(t => t.name === request.name);
      if (!tool) throw new Error("Use a tool from the recorded tools/list response");
      if (mode === "read-only" && tool.annotations?.readOnlyHint !== true) throw new Error("Runtime preparation is pending; this session permits read-only tools");
      record({ method: "tools/call", request });
      const result = await client.callTool({ name: request.name, arguments: request.arguments ?? {} }, undefined, { timeout: 190_000 });
      record({ method: "tools/call", name: request.name, result });
      process.stdout.write(`${JSON.stringify({ name: request.name, result })}\n`);
    } catch (error) {
      const message = error instanceof Error ? error.message : "MCP call failed";
      record({ method: "tools/call", name: request?.name, error: message });
      process.stdout.write(`${JSON.stringify({ name: request?.name, error: message })}\n`);
    }
  }
} finally {
  await client.close();
  lines.close();
  if (rawTerminal) process.stdin.setRawMode(false);
  record({ closed: true });
  closeSync(output);
}
