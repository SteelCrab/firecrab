import { expect, test, type Page, type Route } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";
import { vmPanel } from "./support/vmPanel";

const FINGERPRINT = "SHA256:Zm9vYmFyYmF6cXV4cXV1eGNvcmdlZ3JhdWx0Z2FycGx5";
const PEM = "-----BEGIN OPENSSH PRIVATE KEY-----\nbW9jay1rZXk=\n-----END OPENSSH PRIVATE KEY-----\n";

const RUNNING = vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running", {
  ipv4: "10.42.0.11",
  ipv6: "fd42::11",
  sshHostFingerprint: FINGERPRINT,
  portForwards: [{ hostPort: 8080, guestPort: 80, protocol: "tcp" }],
});
const STOPPED = vm("redis-cache", "5c6d7e8f-9a0b-4c1d-8e2f-3a4b5c6d7e8f", "stopped");
const VMS = [RUNNING, STOPPED];

const routes = {
  [`/api/vms/${RUNNING.id}`]: RUNNING,
  [`/api/vms/${RUNNING.id}/log`]: { consoleLog: "", truncated: false },
  [`/api/vms/${RUNNING.id}/ssh-key`]: (route: Route) => route.fulfill({ body: PEM, contentType: "application/x-pem-file" }),
};

const row = (page: Page, name: string) =>
  page.locator("table.vm-list tbody tr").filter({ has: page.getByRole("button", { name, exact: true }) });
const sshDialog = (page: Page, name: string) => page.getByRole("dialog", { name: `SSH — ${name}` });

async function openList(page: Page) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openDashboard(page, { vms: VMS, routes });
  await expect(row(page, "api-gateway")).toBeVisible();
}

/** Opens the console page with a fake serial socket that answers with a prompt. */
async function openConsole(page: Page, size = { width: 1440, height: 900 }) {
  await page.setViewportSize(size);
  await page.routeWebSocket(`**/ws/vms/${RUNNING.id}/console`, (ws) => {
    ws.send("api-gateway login: root (automatic login)\r\napi-gateway:~# ");
  });
  await openDashboard(page, { vms: VMS, routes, hash: `#/console/${RUNNING.id}` });
  await expect(page.locator(".console-page .console-status")).toHaveText(/Connected/);
}

test.describe("SSH access", () => {
  test("the row menu opens the SSH dialog in three steps", async ({ page }) => {
    await openList(page);
    await row(page, "api-gateway").getByRole("button", { name: "Actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "SSH connect" }).click();

    const dialog = sshDialog(page, "api-gateway");
    await expect(dialog).toBeVisible();
    for (const step of ["Private key", "Connect", "Verify host key"]) {
      await expect(dialog.getByRole("heading", { name: step, exact: true })).toBeVisible();
    }
    await expect(dialog.locator(".console-ssh-code").filter({ hasText: "ssh -i" }).first()).toHaveText("ssh -i firecrab-api-gateway.pem root@10.42.0.11");

    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
  });

  test("the detail panel has no second copy of the SSH controls", async ({ page }) => {
    await openList(page);
    await row(page, "api-gateway").click();
    const panel = vmPanel(page);
    await expect(panel.getByRole("heading", { name: "api-gateway" })).toBeVisible();
    // The SSH section keeps the host key; the key and commands live in the dialog.
    await expect(panel.getByText(FINGERPRINT)).toBeVisible();
    for (const name of ["Download key", "Copy key", "SSH commands"]) {
      await expect(panel.getByRole("button", { name, exact: true })).toHaveCount(0);
    }

    await panel.getByRole("button", { name: "SSH", exact: true }).click();
    const dialog = sshDialog(page, "api-gateway");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Copy key", exact: true })).toHaveCount(1);
    await expect(dialog.getByRole("button", { name: "Download firecrab-api-gateway.pem", exact: true })).toHaveCount(1);

    await dialog.getByRole("button", { name: "Close", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    // Closing the dialog leaves the panel where it was.
    await expect(panel.getByRole("heading", { name: "api-gateway" })).toBeVisible();
  });

  test("copies a command and reveals the key on request", async ({ page }) => {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    await openList(page);
    await row(page, "api-gateway").getByRole("button", { name: "Actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "SSH connect" }).click();
    const dialog = sshDialog(page, "api-gateway");

    const ipv4 = dialog.locator(".console-ssh-block").filter({ has: page.locator(".console-ssh-block-label", { hasText: /^IPv4$/ }) });
    await ipv4.getByRole("button", { name: "Copy IPv4", exact: true }).click();
    await expect(ipv4.getByRole("button", { name: "Copied", exact: true })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("ssh -i firecrab-api-gateway.pem root@10.42.0.11");

    await expect(dialog.locator(".console-ssh-code.is-masked")).toHaveCount(1);
    await dialog.getByRole("button", { name: "Show key", exact: true }).click();
    await expect(dialog.locator(".console-ssh-code.is-masked")).toHaveCount(0);
    await expect(dialog.getByText("bW9jay1rZXk=")).toBeVisible();
  });

  test("a stopped VM explains that it has no address yet", async ({ page }) => {
    await openList(page);
    await row(page, "redis-cache").getByRole("button", { name: "Actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "SSH connect" }).click();
    await expect(sshDialog(page, "redis-cache")).toContainText("No address yet — start the VM.");
  });
});

test.describe("console page", () => {
  test("names the VM and its connection in the header", async ({ page }) => {
    await openConsole(page);
    const header = page.locator(".console-header");
    await expect(header.getByRole("heading", { name: "api-gateway", level: 1 })).toBeVisible();
    await expect(header).toContainText("10.42.0.11");
    await expect(page.getByRole("tab", { name: "Serial" })).toHaveAttribute("aria-selected", "true");
  });

  test("keeps the details beside the terminal and remembers when they are hidden", async ({ page }) => {
    await openConsole(page);
    const details = page.getByRole("complementary", { name: "VM details" });
    await expect(details).toBeVisible();
    const terminal = await page.locator(".console-surface").boundingBox();
    const aside = await details.boundingBox();
    expect(aside!.x).toBeGreaterThanOrEqual(terminal!.x + terminal!.width);
    await expect(details.getByRole("heading", { name: "Resources", exact: true })).toBeVisible();
    await expect(details).toContainText(":80 → :8080/tcp");

    const toggle = page.getByRole("button", { name: "Hide details", exact: true });
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await toggle.click();
    await expect(details).toHaveCount(0);
    await page.reload();
    await expect(page.locator(".console-page .console-status")).toHaveText(/Connected/);
    await expect(page.getByRole("complementary", { name: "VM details" })).toHaveCount(0);
    await page.getByRole("button", { name: "Show details", exact: true }).click();
    await expect(page.getByRole("complementary", { name: "VM details" })).toBeVisible();
  });

  test("puts the details under the terminal on narrow screens", async ({ page }) => {
    await openConsole(page, { width: 820, height: 900 });
    const terminal = await page.locator(".console-surface").boundingBox();
    const aside = await page.getByRole("complementary", { name: "VM details" }).boundingBox();
    expect(aside!.y).toBeGreaterThanOrEqual(terminal!.y + terminal!.height);
  });

  test("the SSH tab shows the same SSH steps", async ({ page }) => {
    await openConsole(page);
    await page.getByRole("tab", { name: "SSH" }).click();
    await expect(page.locator(".console-surface")).toBeHidden();
    for (const step of ["Private key", "Connect", "Verify host key"]) {
      await expect(page.getByRole("heading", { name: step, exact: true })).toBeVisible();
    }
  });

  test("terminal only hides the chrome until Esc", async ({ page }) => {
    await openConsole(page);
    await page.getByRole("button", { name: "Terminal only", exact: true }).click();
    await expect(page.locator(".console-header")).toHaveCount(0);
    await expect(page.getByRole("complementary", { name: "VM details" })).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(page.locator(".console-header")).toBeVisible();
  });

  test("display settings change the terminal font size", async ({ page }) => {
    await openConsole(page);
    await page.getByRole("button", { name: "Display settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Terminal display settings" });
    await settings.getByLabel("Font size").selectOption("16");
    await expect(page.locator(".console-page .xterm-rows")).toHaveCSS("font-size", "16px");
    await page.keyboard.press("Escape");
    await expect(settings).toHaveCount(0);
  });
});
