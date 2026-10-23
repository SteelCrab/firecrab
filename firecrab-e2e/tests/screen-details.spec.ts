import { expect, test, type Page } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const panel = (page: Page, name: string) => page.getByRole("complementary", { name });

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
});

test.describe("list screens with a detail panel", () => {
  test("MicroNetwork shows the selected network in the panel", async ({ page }) => {
    const id = "aaaaaaaa-0000-4000-8000-000000000001";
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [{ id, name: "prod", subnetCidr: "172.31.0.0/24", gateway: "172.31.0.1", internetEnabled: true, uplink: null, ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null }],
        [`/api/micro-networks/${id}`]: {
          id,
          name: "prod",
          subnet: { cidr: "172.31.0.0/24", gateway: "172.31.0.1", usableAddresses: 253, allocatedAddresses: 2, dhcp: "dnsmasq", ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null },
          bridge: { name: "fcbr-prod", attachedTaps: 1 },
          nat: { enabled: true, uplink: "eth0", sourceCidr: "172.31.0.0/24", ipv6SourceCidr: null },
          firewall: { eastWestBlocked: true, crossNetworkBlocked: true, antiSpoofing: true, defaultEgress: "internet" },
          vms: [{ id: "bbbbbbbb-0000-4000-8000-000000000002", name: "web-1", state: "running", ipv4: "172.31.0.10", ipv6: null, egressPolicy: "internet" }],
        },
        "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
      },
    });
    await expect(panel(page, "MicroNetwork detail")).toContainText("Select an item to see its details.");
    await page.getByRole("cell", { name: "prod", exact: true }).click();
    const detail = panel(page, "MicroNetwork detail");
    await expect(detail.getByRole("heading", { name: "prod" })).toBeVisible();
    await expect(detail).toContainText("172.31.0.0/24");
    await expect(detail).toContainText("fcbr-prod");
    await expect(detail.getByRole("region", { name: "Member VMs" })).toContainText("web-1");
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("MicroStorage shows the selected pool in the panel", async ({ page }) => {
    const id = "cccccccc-0000-4000-8000-000000000003";
    await openDashboard(page, {
      hash: "#/storages",
      routes: {
        "/api/micro-storages": [{ id, name: "fast-ssd", path: "/mnt/fast", availableGib: 120, totalGib: 500 }],
        [`/api/micro-storages/${id}`]: { id, name: "fast-ssd", path: "/mnt/fast", availableGib: 120, totalGib: 500, vms: [{ id: "dddddddd-0000-4000-8000-000000000004", name: "db-1", state: "stopped", diskGb: 40 }] },
        "/api/storage/devices": [],
      },
    });
    await page.getByRole("cell", { name: "fast-ssd", exact: true }).click();
    const detail = panel(page, "MicroStorage detail");
    await expect(detail.getByRole("heading", { name: "fast-ssd" })).toBeVisible();
    await expect(detail).toContainText("/mnt/fast");
    await expect(detail).toContainText("120 / 500 GiB");
    await expect(detail.getByRole("region", { name: "VMs" })).toContainText("db-1");
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("Images shows the selected image and the VMs using it", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/images",
      vms: [vm("app-1", "eeeeeeee-0000-4000-8000-000000000005", "running", { template: "ubuntu-24.04" })],
      routes: {
        "/api/images": [{ alias: "ubuntu-24.04", version: "2026.09.1", kernelVersion: "6.1.102", kernelImage: "vmlinux-6.1.102", kernelSha256: "a".repeat(64), rootfsSha256: "b".repeat(64), minDiskGb: 4, rootfsSizeBytes: 1073741824, installed: true, description: "Ubuntu" }],
        "/api/kernels": [],
      },
    });
    await page.locator("table.image-table tbody tr", { hasText: "ubuntu-24.04" }).click();
    const detail = panel(page, "Image detail");
    await expect(detail.getByRole("heading", { name: "ubuntu-24.04" })).toBeVisible();
    await expect(detail).toContainText("2026.09.1");
    await expect(detail).toContainText("6.1.102");
    await expect(detail.getByRole("region", { name: "VMs using it" })).toContainText("app-1");
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("Shells shows revisions and the editor in the panel", async ({ page }) => {
    const id = "ffffffff-0000-4000-8000-000000000006";
    const rev = { id: "rev-2", version: 2, contentSha256: "c".repeat(64), createdAtMs: Date.now(), sizeBytes: 22 };
    await openDashboard(page, {
      hash: "#/shells",
      routes: {
        "/api/shells": [{ id, name: "web-init", description: "Installs nginx", latestVersion: 2, latestRevisionId: "rev-2", contentSha256: "c".repeat(64), createdAtMs: 1, updatedAtMs: 2 }],
        [`/api/shells/${id}`]: { id, name: "web-init", description: "Installs nginx", createdAtMs: 1, updatedAtMs: 2, revisions: [rev, { ...rev, id: "rev-1", version: 1 }], latestContent: "#!/bin/sh\necho hello\n" },
        [`/api/shells/${id}/revisions/rev-2`]: { shellId: id, revisionId: "rev-2", version: 2, contentSha256: "c".repeat(64), content: "#!/bin/sh\necho hello\n", createdAtMs: 2 },
      },
    });
    await page.getByRole("cell", { name: /web-init/ }).click();
    const detail = panel(page, "Shell detail");
    await expect(detail.getByRole("heading", { name: "web-init" })).toBeVisible();
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v2");
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v1");
    await expect(detail.locator("#shell-revise")).toHaveValue("#!/bin/sh\necho hello\n");
    await expect(detail.getByRole("button", { name: "Publish new revision" })).toBeVisible();
  });

  test("MicroService opens its forms and runtime in the panel", async ({ page }) => {
    const id = "99999999-0000-4000-8000-000000000007";
    const service = { id, name: "orders", image: "ghcr.io/team/orders:1.0", runtime: "vm", port: 8080, healthPath: "/health", domain: null, ram: 512, cpu: 1, replicas: 1, startupTimeout: 300, drainTimeout: 30, env: {}, secrets: {} };
    await openDashboard(page, {
      hash: "#/services",
      routes: {
        "/api/micro-services": [service],
        "/api/pipe/storage": [],
        "/api/security-groups": [],
        "/api/micro-networks": [],
        [`/api/micro-services/${id}`]: service,
        [`/api/micro-services/${id}/deployment`]: (route) => route.fulfill({ json: null }),
      },
    });
    const detail = panel(page, "MicroService detail");
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await expect(detail.getByRole("heading", { name: "New MicroService" })).toBeVisible();
    await detail.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(detail.getByRole("heading", { name: "New MicroService" })).toHaveCount(0);

    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await expect(detail.getByRole("heading", { name: "Edit: orders", exact: true })).toBeVisible();
    // Run stays disabled while a form is open, as before.
    await expect(page.getByRole("button", { name: "Run orders", exact: true })).toBeDisabled();
    await detail.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(detail.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
    await expect(detail.getByRole("status")).toHaveText("Not deployed");
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("Esc on the image row menu leaves the panel open", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/images",
      routes: {
        "/api/images": [{ alias: "ubuntu-24.04", version: "2026.09.1", kernelVersion: "6.1.102", kernelImage: "vmlinux-6.1.102", kernelSha256: "a".repeat(64), rootfsSha256: "b".repeat(64), minDiskGb: 4, rootfsSizeBytes: 1073741824, installed: true, description: "Ubuntu" }],
        "/api/kernels": [],
      },
    });
    const imageRow = page.locator("table.image-table tbody tr", { hasText: "ubuntu-24.04" });
    await imageRow.click();
    const detail = panel(page, "Image detail");
    await expect(detail.getByRole("heading", { name: "ubuntu-24.04" })).toBeVisible();
    await imageRow.locator("button.options-menu-trigger").click();
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(detail.getByRole("heading", { name: "ubuntu-24.04" })).toBeVisible();
  });

  for (const width of [1280, 1440]) {
    test(`list tables fit beside the panel at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      const netId = "aaaaaaaa-0000-4000-8000-000000000011";
      const svcId = "99999999-0000-4000-8000-000000000012";
      const service = { id: svcId, name: "orders-api", image: "ghcr.io/example-team/orders-api:2026.10.1", runtime: "vm", port: 8080, healthPath: "/health", domain: "orders.example.com", ram: 1024, cpu: 2, replicas: 3, startupTimeout: 300, drainTimeout: 30, env: {}, secrets: {} };
      await openDashboard(page, {
        hash: "#/networks",
        routes: {
          "/api/micro-networks": [{ id: netId, name: "production-east", subnetCidr: "172.31.0.0/24", gateway: "172.31.0.1", internetEnabled: true, uplink: "eth0", ipv6Cidr: "fd42:31::/64", ipv6Gateway: "fd42:31::1", ipv6AddressMode: "slaac", ipv6Egress: "nat66" }],
          "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
          "/api/micro-services": [service],
          [`/api/micro-services/${svcId}/deployment`]: (route) => route.fulfill({ json: null }),
        },
      });
      const fits = async () => page.locator(".screen-body .table-scroll").first().evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
      await expect(panel(page, "MicroNetwork detail")).toBeVisible();
      expect(await fits()).toBe(true);

      await page.getByRole("button", { name: "MicroService", exact: true }).click();
      await page.getByRole("button", { name: "Run orders-api", exact: true }).click();
      await expect(panel(page, "MicroService detail").getByRole("heading", { name: "Run: orders-api" })).toBeVisible();
      expect(await fits()).toBe(true);
    });
  }
});
