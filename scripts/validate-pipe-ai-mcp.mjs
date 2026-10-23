// A real MCP SDK client, with a disposable REST fixture. No provider or VM is used.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

const [binary, sdkRoot] = process.argv.slice(2);
if (!binary || !sdkRoot) throw new Error("Usage: node scripts/validate-pipe-ai-mcp.mjs /absolute/firecrab /absolute/node_modules/@modelcontextprotocol/sdk");
const { Client } = await import(pathToFileURL(join(resolve(sdkRoot), "dist/esm/client/index.js")));
const { StdioClientTransport } = await import(pathToFileURL(join(resolve(sdkRoot), "dist/esm/client/stdio.js")));
const borrowedId = randomUUID();
const network = (id, name, subnetCidr) => ({ id, name, subnetCidr, gateway: subnetCidr.replace(".0/24", ".1"), internetEnabled: true, uplink: null, ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null });
let networks = [network(borrowedId, "borrowed", "172.30.0.0/24")];
let vms = [];
const trace = [];
const rest = createServer(async (request, response) => {
  const path = new URL(request.url, "http://fixture").pathname;
  let text = "";
  for await (const chunk of request) text += chunk;
  const input = text ? JSON.parse(text) : null;
  trace.push({ method: request.method, path });
  let result;
  let status = 200;
  if (request.method === "GET" && path === "/api/images") result = [];
  else if (request.method === "GET" && path === "/api/pipe/ai/connections") result = [];
  else if (request.method === "GET" && path.endsWith("/claude-code/status")) result = { status: "signed_out" };
  else if (request.method === "GET" && path.endsWith("/model")) result = null;
  else if (request.method === "GET" && path === "/api/micro-networks") result = networks;
  else if (request.method === "POST" && path === "/api/micro-networks") {
    result = network(randomUUID(), input.name, input.subnetCidr); networks.push(result); status = 201;
  } else if (request.method === "GET" && path === "/api/vms") result = vms;
  else if (request.method === "POST" && path === "/api/vms") {
    result = { id: randomUUID(), ...input, state: "created", templateVersion: "fixture", reconciliation: null, startupStep: null,
      ipv4: "172.31.0.2", ipv6: null, mac: "02:00:00:00:00:01", hostname: "fixture", startupTimeline: [],
      storageRoot: "default", cpuUsagePercent: null, memoryUsedMib: null, memoryTotalMib: null,
      memoryUsedPercent: null, usageHistory: [], shellRefs: [], portForwards: [], env: { SECRET: "omit-from-model-context" } };
    vms.push(result); status = 201;
  } else if (/^\/api\/vms\/[^/]+/.test(path)) {
    const id = path.split("/")[3];
    const vm = vms.find(vm => vm.id === id);
    assert.ok(vm);
    if (request.method === "DELETE") { assert.equal(vm.state, "stopped"); vms = vms.filter(vm => vm.id !== id); status = 204; }
    else { if (path.endsWith("/start")) vm.state = "running"; if (path.endsWith("/stop")) vm.state = "stopped"; result = vm; }
  } else if (request.method === "DELETE" && path.startsWith("/api/micro-networks/")) {
    const id = path.split("/").at(-1); assert.notEqual(id, borrowedId);
    assert.ok(vms.every(vm => vm.microNetworkId !== id)); networks = networks.filter(network => network.id !== id); status = 204;
  } else throw new Error(`Unexpected REST request: ${request.method} ${path}`);
  response.writeHead(status, { "content-type": "application/json" });
  response.end(status === 204 ? "" : JSON.stringify(result));
});
await new Promise(resolve => rest.listen(0, "127.0.0.1", resolve));
const client = new Client({ name: "firecrab-414-validation", version: "1.0.0" }, { capabilities: {} });
const transport = new StdioClientTransport({ command: resolve(binary), args: ["--api", `http://127.0.0.1:${rest.address().port}`, "mcp"], stderr: "pipe" });
try {
  await client.connect(transport);
  const { tools } = await client.listTools();
  assert.ok(tools.some(tool => tool.name === "ai_respond"));
  async function call(name, args = {}) {
    const result = await client.callTool({ name, arguments: args });
    assert.ok(!result.isError, JSON.stringify(result));
    assert.ok(result.structuredContent);
    return result.structuredContent;
  }
  await call("images_list");
  assert.equal((await call("networks_list")).items.length, 1);
  const args = { operationId: randomUUID(), name: "sdk-proof", subnetCidr: "172.31.0.0/24" };
  const createdNetwork = await call("network_create", args);
  assert.equal((await call("network_create", args)).id, createdNetwork.id);
  const vm = await call("vm_create", { operationId: randomUUID(), name: "sdk-proof", template: "ubuntu-fixture", microNetworkId: createdNetwork.id, cpu: 1, ram: 512, diskGb: 2 });
  assert.ok(!JSON.stringify(vm).includes("omit-from-model-context"));
  await call("vm_start", { id: vm.id });
  assert.equal((await call("vm_wait", { id: vm.id, state: "running", timeoutSeconds: 1 })).reached, true);
  assert.equal((await client.callTool({ name: "network_delete", arguments: { id: borrowedId } })).isError, true);
  assert.equal((await call("owned_resources")).resources.length, 2);
  await call("vm_stop", { id: vm.id });
  await call("vm_delete", { id: vm.id });
  await call("network_delete", { id: createdNetwork.id });
  assert.equal((await call("owned_resources")).resources.length, 0);
  await call("ai_connections_list");
  assert.equal((await call("ai_claude_code_status", { id: randomUUID() })).status, "signed_out");
  await call("ai_default_get", { projectId: randomUUID() });
  assert.deepEqual(vms, []);
  assert.equal(networks.length, 1);
  assert.equal(networks[0].id, borrowedId);
  assert.equal(trace.filter(item => item.method === "POST" && item.path === "/api/micro-networks").length, 1);
  console.log(JSON.stringify({ mode: "real-mcp-client/mock-rest", toolCount: tools.length, trace, finalState: { vms, networks }, passed: true }, null, 2));
} finally {
  await client.close();
  rest.closeAllConnections();
  await new Promise(resolve => rest.close(resolve));
}
