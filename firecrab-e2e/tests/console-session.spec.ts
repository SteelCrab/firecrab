import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import { apiUrl, NETWORK_READY, SKIP_GUEST_BOOT } from "../src/constants.js";
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

/**
 * Issue #303 browser E2E — `exit` in the guest shell ends the terminal
 * session instead of silently reattaching to the shell the guest respawns.
 *
 *   npm --prefix firecrab-e2e run test:dashboard   # @dashboard only, no guest
 *   npm --prefix firecrab-e2e run test:console     # also boots a real guest
 *
 * The @dashboard tests fake the console WebSocket, so they need neither KVM
 * nor the net helper. The guest test boots the local OCI fixture and needs
 * both (`./scripts/dev-net-helper.sh`).
 */

/** Close code the API sends when the guest session ended (#303). */
const SESSION_ENDED_CLOSE_CODE = 4000;

const MOCK_VM_ID = "30330330-3303-4303-8303-303303303303";
const CONSOLE_REGISTRY_PORT = Number.parseInt(
  process.env.FIRECRAB_OCI_CONSOLE_E2E_PORT ?? "15558",
  10,
);
const CONSOLE_VM_NAME = "console-e2e";
const CONSOLE_NETWORK_NAME = "console-e2e";
const CONSOLE_NETWORK_CIDR = "172.30.95.0/24";

async function openEnglish(page: Page, hash: string): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("firecrab.locale", "en");
  });
  await page.goto(hash);
}

function consoleStatus(page: Page) {
  return page.locator(".console-page .console-status");
}

async function typeInTerminal(page: Page, line: string): Promise<void> {
  await page.locator(".console-page .xterm").click();
  await page.keyboard.type(line);
  await page.keyboard.press("Enter");
}

/** Serves one running VM's REST reads so the console page renders. */
async function mockVm(page: Page): Promise<void> {
  const vm = {
    id: MOCK_VM_ID,
    name: "console-mock",
    state: "running",
    template: "alpine-3.24.1",
    templateVersion: "alpine-3.24.1-v5",
    cpu: 1,
    ram: 512,
    diskGb: 2,
    startupStep: null,
    startupTimeline: [],
    egressPolicy: "isolated",
    ipv4: "172.30.95.2",
    ipv6: null,
    mac: "02:fc:00:00:00:02",
    hostname: "fc-mock",
    microNetworkId: "00000000-0000-0000-0000-000000000001",
    storageRoot: "default",
    cpuUsagePercent: null,
    memoryUsedMib: null,
    memoryTotalMib: null,
    memoryUsedPercent: null,
    usageHistory: [],
  };
  await page.route(`**/api/vms/${MOCK_VM_ID}`, (route) => route.fulfill({ json: vm }));
  await page.route(`**/api/vms/${MOCK_VM_ID}/log`, (route) =>
    route.fulfill({ json: { consoleLog: "", truncated: false } }),
  );
}

test.describe("terminal session end @dashboard", () => {
  test("a guest exit stops at Session ended and New session attaches again", async ({ page }) => {
    await mockVm(page);
    let connections = 0;
    await page.routeWebSocket(`**/ws/vms/${MOCK_VM_ID}/console`, (ws) => {
      connections += 1;
      ws.send(
        connections === 1
          ? "fc-mock login: root (automatic login)\r\nfc-mock:~# "
          : "\r\n=== session ended — starting a new one ===\r\n\r\nfc-mock:~# ",
      );
      let typed = "";
      ws.onMessage((message) => {
        typed += typeof message === "string" ? message : message.toString("utf8");
        if (typed.includes("exit\r")) {
          ws.close({ code: SESSION_ENDED_CLOSE_CODE, reason: "session_ended" });
        }
      });
    });

    await openEnglish(page, `/#/console/${MOCK_VM_ID}`);
    await expect(consoleStatus(page)).toHaveText(/Connected/);

    await typeInTerminal(page, "exit");

    await expect(consoleStatus(page)).toHaveText(/Session ended/);
    await expect(page.locator(".console-page .xterm-rows")).toContainText("Guest session ended");
    // Longer than the first reconnect backoff: the page must stay detached.
    await page.waitForTimeout(2500);
    expect(connections).toBe(1);

    await page.getByRole("button", { name: "New session" }).click();
    await expect(consoleStatus(page)).toHaveText(/Connected/);
    expect(connections).toBe(2);
    await expect(page.locator(".console-page .xterm-rows")).toContainText(
      "session ended — starting a new one",
    );
  });

  test("any other close still reconnects on its own", async ({ page }) => {
    await mockVm(page);
    let connections = 0;
    await page.routeWebSocket(`**/ws/vms/${MOCK_VM_ID}/console`, (ws) => {
      connections += 1;
      ws.send("fc-mock:~# ");
      if (connections === 1) {
        ws.close({ code: 1011, reason: "server restart" });
      }
    });

    await openEnglish(page, `/#/console/${MOCK_VM_ID}`);

    await expect.poll(() => connections, { timeout: 10_000 }).toBe(2);
    await expect(consoleStatus(page)).toHaveText(/Connected/);
  });
});

test.describe("terminal session end in a booted guest", () => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set. Unset it (and run ./scripts/dev-net-helper.sh) to boot a guest.",
  );
  test.describe.configure({ mode: "serial" });

  const api = new ApiCleanup();
  let registry: LocalOciRegistry | null = null;

  async function cleanup(): Promise<void> {
    const alias = registry?.announcement.alias;
    if (alias) {
      await api.deleteOwnedVms(alias, CONSOLE_VM_NAME);
      await api.deleteImportedImage(alias);
    }
    await api.deleteNetworksByName([CONSOLE_NETWORK_NAME]);
  }

  test.beforeAll(async () => {
    registry = await startLocalOciRegistry(CONSOLE_REGISTRY_PORT);
    await cleanup();
  });

  test.afterAll(async () => {
    await cleanup();
    await registry?.stop();
  });

  test("exit in the guest shell ends the session and the VM keeps running", async ({ page }) => {
    const { reference, alias } = registry!.announcement;
    const vmId = await bootFixtureGuest(reference, alias);

    await openEnglish(page, `/#/console/${vmId}`);
    await expect(consoleStatus(page)).toHaveText(/Connected/);
    await typeInTerminal(page, "");
    await typeInTerminal(page, "exit");

    await expect(consoleStatus(page)).toHaveText(/Session ended/, { timeout: 30_000 });
    expect((await api.getVm(vmId))?.state).toBe("running");

    await page.getByRole("button", { name: "New session" }).click();
    await expect(consoleStatus(page)).toHaveText(/Connected/);
    await expect(page.locator(".console-page .xterm-rows")).toContainText(
      "session ended — starting a new one",
      { timeout: 15_000 },
    );
  });
});

async function request(method: string, pathname: string, body?: unknown): Promise<unknown> {
  const response = await fetch(`${apiUrl()}${pathname}`, {
    method,
    headers: body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${method} ${pathname} → ${response.status}: ${text}`);
  }
  return text ? JSON.parse(text) : null;
}

async function poll<T>(what: string, timeoutMs: number, probe: () => Promise<T | null>): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await probe();
    if (value !== null) return value;
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** Imports the fixture, then creates and starts one VM from it. */
async function bootFixtureGuest(reference: string, alias: string): Promise<string> {
  await request("POST", "/api/oci/import", { reference });
  await poll("the fixture import", 180_000, async () => {
    const job = (await request("GET", `/api/oci/import/${encodeURIComponent(alias)}`)) as {
      status: string;
      log?: string;
    };
    if (job.status === "failed") throw new Error(`fixture import failed:\n${job.log ?? ""}`);
    return job.status === "succeeded" ? job : null;
  });
  const image = (await request("GET", `/api/images/${encodeURIComponent(alias)}`)) as {
    minDiskGb?: number;
  };
  const network = (await request("POST", "/api/micro-networks", {
    name: CONSOLE_NETWORK_NAME,
    subnetCidr: CONSOLE_NETWORK_CIDR,
    internetEnabled: false,
  })) as { id: string };
  const vm = (await request("POST", "/api/vms", {
    name: CONSOLE_VM_NAME,
    template: alias,
    cpu: 1,
    ram: 512,
    diskGb: image.minDiskGb ?? 2,
    egressPolicy: "isolated",
    microNetworkId: network.id,
  })) as { id: string };
  await request("POST", `/api/vms/${vm.id}/start`);
  await poll("the guest to run", 240_000, async () => {
    const current = (await request("GET", `/api/vms/${vm.id}`)) as { state: string };
    if (current.state === "error") throw new Error(`${CONSOLE_VM_NAME} entered error`);
    return current.state === "running" ? current : null;
  });
  const log = (await request("GET", `/api/vms/${vm.id}/log`)) as { consoleLog: string };
  expect(log.consoleLog).toContain(NETWORK_READY);
  return vm.id;
}
