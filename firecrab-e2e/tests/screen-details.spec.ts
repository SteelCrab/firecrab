import { expect, test, type Page, type Route } from "@playwright/test";
import { serviceAction } from "./support/serviceSetup";
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
    await expect(page.locator("#detail-panel")).toBeHidden();
    await page.getByRole("cell", { name: /^prod(\s|$)/ }).click();
    const detail = panel(page, "MicroNetwork detail");
    await expect(detail.getByRole("heading", { name: "prod" })).toBeVisible();
    await expect(detail).toContainText("172.31.0.0/24");
    await expect(detail).toContainText("fcbr-prod");
    await expect(detail.getByRole("region", { name: "Member VMs" })).toContainText("web-1");
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("list rows use an icon-only Details button", async ({ page }) => {
    const id = "aaaaaaaa-0000-4000-8000-000000000001";
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [{ id, name: "prod", subnetCidr: "172.31.0.0/24", gateway: "172.31.0.1", internetEnabled: true, uplink: null, ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null }],
        "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
      },
    });
    const details = page.locator("table.wasm-table tbody tr").first().getByRole("button", { name: "Details", exact: true });
    await expect(details).toHaveText("");
    await expect(details).toHaveAttribute("title", "Details");
  });

  test("uses flat tables with hover row actions and a shared empty state", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [{ id: "n1", name: "default", subnetCidr: "172.30.35.0/24", gateway: "172.30.35.1", internetEnabled: true }],
        "/api/micro-storages": [],
      },
    });
    const table = page.locator("table.wasm-table");
    await expect(table.locator("th").first()).toHaveCSS("height", "51px");
    const first = table.locator("tbody tr").first();
    expect((await first.boundingBox())!.height).toBeGreaterThanOrEqual(75);
    const details = first.getByRole("button", { name: "Details", exact: true });
    await expect(details).toHaveCSS("opacity", "0");
    await first.hover();
    await expect(details).toHaveCSS("opacity", "1");
    await page.goto("/#/storages");
    await expect(page.locator(".empty-state .empty-state-title")).toBeVisible();
  });

  test("marks the selected row at once", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [
          { id: "n1", name: "default", subnetCidr: "172.30.35.0/24", gateway: "172.30.35.1", internetEnabled: true },
          { id: "n2", name: "staging", subnetCidr: "172.30.36.0/24", gateway: "172.30.36.1", internetEnabled: false },
        ],
        "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
      },
    });
    const rows = page.locator("table.wasm-table tbody tr");
    await rows.nth(1).getByRole("cell", { name: /^staging(\s|$)/ }).click();
    await expect(rows.nth(1)).toHaveAttribute("aria-selected", "true");
    await expect(rows.nth(0)).toHaveAttribute("aria-selected", "false");
    const cell = rows.nth(1).locator("td").first();
    await expect(cell).toHaveCSS("background-color", "rgb(246, 246, 248)");
    await expect(cell).toHaveCSS("box-shadow", /rgb\(232, 73, 47\) 2px 0px 0px 0px inset/);
  });

  test("a sub-tab click closes the selection", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [{ id: "n1", name: "default", subnetCidr: "172.30.35.0/24", gateway: "172.30.35.1", internetEnabled: true }],
        "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
      },
    });
    await page.getByRole("cell", { name: /^default(\s|$)/ }).click();
    await expect(page.locator("#detail-panel")).toBeVisible();
    // No screen has sub-tabs yet: add the markup `render::subtabs` makes inside the content.
    await page.locator("#content").evaluate((el) => el.insertAdjacentHTML("afterbegin", '<div class="subtabs" role="tablist" aria-label="Sections"><button type="button" role="tab" class="subtab" aria-selected="false" data-action="subtab" data-id="networks:other">Other <em>0</em></button></div>'));
    await page.getByRole("tab", { name: "Other 0" }).click();
    await expect(page.locator("#detail-panel")).toBeHidden();
    await expect(page.locator("table.wasm-table tbody tr").first()).toHaveAttribute("aria-selected", "false");
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
    await page.getByRole("cell", { name: /^fast-ssd(\s|$)/ }).click();
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
    await expect(detail.getByRole("tab", { name: /^Content/ })).toHaveAttribute("aria-selected", "true");
    await expect(detail.getByRole("region", { name: "Latest revision" })).toContainText("echo hello");
    await detail.getByRole("tab", { name: /^Revisions/ }).click();
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v2");
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v1");
    await detail.getByRole("button", { name: "Publish revision", exact: true }).click();
    await expect(detail.getByLabel("Shell script", { exact: true })).toHaveValue("#!/bin/sh\necho hello\n");
    await expect(detail.getByRole("button", { name: "Save", exact: true })).toBeVisible();
  });

  test("publishing a shell revision updates its version and content without reloading", async ({ page }) => {
    const id = "ffffffff-0000-4000-8000-000000000006";
    let content = "#!/bin/sh\necho previous\n";
    let version = 2;
    const summary = () => ({ id, name: "web-init", description: "Installs nginx", latestVersion: version, latestRevisionId: `rev-${version}`, contentSha256: "c".repeat(64), createdAtMs: 1, updatedAtMs: version });
    const revisions = [{ id: "rev-2", version: 2, contentSha256: "c".repeat(64), createdAtMs: 2, sizeBytes: content.length }];
    await openDashboard(page, {
      hash: "#/shells",
      routes: {
        "/api/shells": route => route.fulfill({ json: [summary()] }),
        [`/api/shells/${id}`]: route => route.fulfill({ json: { id, name: "web-init", description: "Installs nginx", createdAtMs: 1, updatedAtMs: version, revisions, latestContent: content } }),
        [`/api/shells/${id}/revisions`]: route => {
          content = route.request().postDataJSON().content;
          version += 1;
          const revision = { id: `rev-${version}`, version, contentSha256: "d".repeat(64), createdAtMs: version, sizeBytes: content.length };
          revisions.unshift(revision);
          return route.fulfill({ status: 201, json: { shellId: id, revisionId: revision.id, version, contentSha256: revision.contentSha256, content, createdAtMs: version } });
        },
      },
    });
    const row = page.locator("table.shell-table tbody tr", { hasText: "web-init" });
    await row.click();
    const detail = panel(page, "Shell detail");
    await expect(detail.getByRole("region", { name: "Latest revision" })).toContainText("echo previous");
    await detail.getByRole("button", { name: "Publish revision", exact: true }).click();
    const updated = "#!/bin/sh\necho updated\n";
    await detail.getByLabel("Shell script", { exact: true }).fill(updated);
    await detail.getByRole("button", { name: "Save", exact: true }).click();
    await expect(row).toContainText("v3");
    await expect(detail.locator(".detail-meta")).toContainText("v3");
    await expect(detail.getByRole("region", { name: "Latest revision" })).toContainText("echo updated");
    await detail.getByRole("tab", { name: /^Revisions/ }).click();
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v3");
    await expect(detail.getByRole("region", { name: "Revisions" })).toContainText("v2");
    await detail.getByRole("button", { name: "Publish revision", exact: true }).click();
    await expect(detail.getByLabel("Shell script", { exact: true })).toHaveValue(updated);
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
    // Creating a service is a page of its own; Cancel comes back to the list.
    await page.locator(".screen-header").getByRole("link", { name: "New MicroService", exact: true }).click();
    await expect(page).toHaveURL(/#\/services\/new$/);
    await expect(page.getByRole("heading", { name: "New MicroService", level: 2 })).toBeVisible();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page).toHaveURL(/#\/services$/);

    await serviceAction(page, "orders", "Edit");
    await expect(detail.getByRole("heading", { name: "Edit MicroService", exact: true })).toBeVisible();
    // Opening another runtime panel must not discard an active form.
    await expect(page.getByRole("button", { name: "Open orders", exact: true })).toBeDisabled();
    await detail.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Opening a service shows its tabs; the Deploy form starts from the head.
    await expect(detail.getByRole("heading", { name: "orders", exact: true })).toBeVisible();
    await expect(detail.getByRole("tab")).toHaveText(["Overview", "Instances", "Releases", "Settings"]);
    await expect(detail.getByRole("status")).toHaveText("Not deployed");
    await detail.getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(detail.getByRole("heading", { name: "Deploy MicroService", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("image actions stay available in the selected detail panel", async ({ page }) => {
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
    await expect(detail.getByRole("button", { name: "Download package", exact: true })).toBeVisible();
    await expect(detail.getByRole("button", { name: "Delete image", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(detail).toHaveCount(0);
  });

  test("a panel form keeps Cancel and the submit button in a bar at the bottom of the panel", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/networks",
      routes: {
        "/api/micro-networks": [{ id: "aaaaaaaa-0000-4000-8000-000000000001", name: "prod", subnetCidr: "172.31.0.0/24", gateway: "172.31.0.1", internetEnabled: true, uplink: null, ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null }],
        "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
      },
    });
    await page.getByRole("button", { name: /^(Create|New network)$/ }).first().click();
    const detail = page.locator("#detail-panel");
    await expect(detail.getByRole("heading", { name: "New network" })).toHaveCount(1);
    const footer = detail.locator(".form-footer");
    const cancel = footer.getByRole("button", { name: "Cancel", exact: true });
    const submit = footer.locator("button[type=submit]");
    await expect(footer.locator("button").first()).toHaveText("Cancel");
    await expect(cancel).toHaveClass(/ghost/);
    // A short form and a tall form: the bar ends where the panel ends.
    for (const height of [900, 520]) {
      await page.setViewportSize({ width: 1440, height });
      const bottoms = await page.evaluate(() => ({
        panel: document.querySelector("#detail-panel")!.getBoundingClientRect().bottom,
        footer: document.querySelector("#detail-panel .form-footer")!.getBoundingClientRect().bottom,
      }));
      expect(Math.abs(bottoms.footer - bottoms.panel)).toBeLessThanOrEqual(1);
    }
    const [cancelBox, submitBox] = [await cancel.boundingBox(), await submit.boundingBox()];
    expect(cancelBox!.x).toBeLessThan(submitBox!.x);
  });

  const networkRoutes = (onCreate: (route: Route) => unknown) => ({
    "/api/micro-networks": (route: Route) =>
      route.request().method() === "POST" ? onCreate(route) : route.fulfill({ json: [] }),
    "/api/network": { bridgeName: "fcbr0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] },
  });
  const openNewNetwork = async (page: Page) => {
    await page.getByRole("button", { name: /^(Create|New network)$/ }).first().click();
    const detail = page.locator("#detail-panel");
    await expect(detail.getByRole("heading", { name: "New network" })).toHaveCount(1);
    return detail;
  };

  test("a panel form shows short fixed choices as radio groups and submits the checked ones", async ({ page }) => {
    const posts: Record<string, unknown>[] = [];
    await openDashboard(page, {
      hash: "#/networks",
      routes: networkRoutes((route) => {
        posts.push(route.request().postDataJSON());
        return route.fulfill({ status: 201, json: { id: "n1", name: "lab", subnetCidr: "10.88.0.0/24", gateway: "10.88.0.1", internetEnabled: false } });
      }),
    });
    const detail = await openNewNetwork(page);
    // Two choices: a segmented control. The checked one is the form's value; arrow keys move it.
    const internet = detail.getByRole("radiogroup", { name: "Internet enabled", exact: true });
    await expect(internet).toHaveClass(/segmented/);
    await expect(internet.getByRole("radio", { name: "Enabled", exact: true })).toBeChecked();
    await internet.getByRole("radio", { name: "Enabled", exact: true }).focus();
    await page.keyboard.press("ArrowRight");
    await expect(internet.getByRole("radio", { name: "Blocked", exact: true })).toBeChecked();
    // Three choices (with "Default" for an optional field): chips.
    const mode = detail.getByRole("radiogroup", { name: "IPv6 address mode", exact: true });
    await expect(mode).toHaveClass(/chips/);
    await expect(mode.getByRole("radio", { name: "Default", exact: true })).toBeChecked();
    // The invisible radio covers its whole option, so a click anywhere on the chip lands on it.
    await mode.getByRole("radio", { name: "DHCPv6", exact: true }).click();
    await expect(mode.getByRole("radio", { name: "DHCPv6", exact: true })).toBeChecked();
    // The optional fields say so; the required ones do not.
    await expect(detail.locator(".field", { has: page.locator("#field-uplink") }).locator(".field-optional")).toHaveText("Optional");
    await expect(detail.locator(".field", { has: page.locator("#field-name") }).locator(".field-optional")).toHaveCount(0);
    await detail.getByLabel("Name", { exact: true }).fill("lab");
    await detail.getByLabel("IPv4 subnet (CIDR)", { exact: true }).fill("10.88.0.0/24");
    await detail.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => posts.length).toBe(1);
    expect(posts[0]).toMatchObject({ name: "lab", internetEnabled: false, ipv6AddressMode: "dhcpv6" });
  });

  test("keyboard focus shows a ring on the checked option of a segmented control and of a chip group", async ({ page }) => {
    await openDashboard(page, { hash: "#/networks", routes: networkRoutes((route) => route.fulfill({ status: 500 })) });
    const detail = await openNewNetwork(page);
    // Tab from the field before the group: focus lands on the group's checked radio.
    const focusedSpanShadow = () => page.evaluate(() => {
      const radio = document.activeElement as HTMLInputElement;
      return { name: radio.name, checked: radio.checked, shadow: getComputedStyle(radio.nextElementSibling!).boxShadow };
    });
    await detail.getByLabel("IPv4 subnet (CIDR)", { exact: true }).focus();
    await page.keyboard.press("Tab");
    const segmented = await focusedSpanShadow();
    expect(segmented).toMatchObject({ name: "internetEnabled", checked: true });
    expect(segmented.shadow).toContain("rgba(232, 73, 47, 0.16) 0px 0px 0px 3px");
    // The selected segment keeps its own shadow.
    expect(segmented.shadow).toContain("rgba(0, 0, 0, 0.08) 0px 1px 2px");
    // Then the uplink dropdown, the IPv6 subnet box and the chip group.
    await page.keyboard.press("Tab");
    await page.keyboard.press("Tab");
    await page.keyboard.press("Tab");
    const chips = await focusedSpanShadow();
    expect(chips).toMatchObject({ name: "ipv6AddressMode", checked: true });
    expect(chips.shadow).toContain("rgba(232, 73, 47, 0.16) 0px 0px 0px 3px");
    // A mouse click does not draw a ring.
    await detail.getByRole("radiogroup", { name: "Internet enabled", exact: true }).getByRole("radio", { name: "Blocked", exact: true }).click();
    expect((await focusedSpanShadow()).shadow).not.toContain("rgba(232, 73, 47, 0.16)");
  });

  test("a submit error shows in the banner while a tall form is scrolled to the top, above the pinned footer", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 480 });
    await openDashboard(page, {
      hash: "#/networks",
      routes: networkRoutes((route) => route.fulfill({ status: 422, json: { error: { message: "Subnet overlaps an existing network." } } })),
    });
    const detail = await openNewNetwork(page);
    await detail.getByLabel("Name", { exact: true }).fill("lab");
    await detail.getByLabel("IPv4 subnet (CIDR)", { exact: true }).fill("10.88.0.0/24");
    // The form is taller than the panel; scroll it to the top, away from the footer.
    const body = detail.locator(".detail-content");
    expect(await body.evaluate((el) => el.scrollHeight - el.clientHeight)).toBeGreaterThan(0);
    await body.evaluate((el) => { el.scrollTop = 0; });
    await detail.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert").filter({ hasText: "Subnet overlaps an existing network." })).toBeInViewport();
    await expect(detail.locator(".form-footer")).toBeInViewport();
    await expect(detail.getByRole("button", { name: "Save", exact: true })).toBeInViewport();
    // The form keeps what was typed.
    await expect(detail.getByLabel("Name", { exact: true })).toHaveValue("lab");
  });

  test("the form's result area sits inside the form, right above the footer", async ({ page }) => {
    await openDashboard(page, { hash: "#/networks", routes: networkRoutes((route) => route.fulfill({ status: 500 })) });
    const detail = await openNewNetwork(page);
    await expect(detail.locator("#editor #form-result")).toBeHidden();
    await page.evaluate(() => {
      document.getElementById("form-result")!.innerHTML = "<p role=\"status\">Checked</p>";
      const body = document.querySelector("#detail-panel .detail-content")!;
      body.scrollTop = body.scrollHeight;
    });
    const boxes = await page.evaluate(() => {
      const result = document.getElementById("form-result")!.getBoundingClientRect();
      const footer = document.querySelector("#editor .form-footer")!.getBoundingClientRect();
      const panel = document.getElementById("detail-panel")!.getBoundingClientRect();
      return { gap: footer.top - result.bottom, footerBottom: footer.bottom, panelBottom: panel.bottom };
    });
    // One enlarged field gap and nothing else between the message and the footer.
    expect(boxes.gap).toBeGreaterThanOrEqual(0);
    expect(boxes.gap).toBeLessThanOrEqual(24.5);
    expect(Math.abs(boxes.footerBottom - boxes.panelBottom)).toBeLessThanOrEqual(1);
  });

  test("the image panel inspects and imports an OCI reference on entry", async ({ page }) => {
    let inspected = "";
    let imported: unknown;
    await openDashboard(page, {
      hash: "#/images",
      routes: {
        "/api/images": [],
        "/api/kernels": [],
        "/api/oci/inspect": (route) => {
          inspected = new URL(route.request().url()).searchParams.get("reference") ?? "";
          return route.fulfill({ json: { reference: inspected, digest: "sha256:abc" } });
        },
        "/api/oci/import": (route) => {
          imported = route.request().postDataJSON();
          return route.fulfill({ status: 202, json: { alias: "nginx", status: "running" } });
        },
        "/api/oci/import/nginx": { alias: "nginx", status: "running" },
      },
    });
    const detail = page.locator("#detail-panel");
    await expect(detail).toBeVisible();
    await page.getByRole("link", { name: "Networks", exact: true }).click();
    await expect(detail).toBeHidden();
    await page.getByRole("link", { name: "Images", exact: true }).click();
    await expect(detail.getByLabel("OCI reference", { exact: true })).toBeVisible();
    await detail.getByLabel("OCI reference", { exact: true }).fill("docker.io/library/nginx:1.27");
    await detail.getByRole("button", { name: "Inspect image", exact: true }).click();
    await expect(detail.locator("#form-result")).toContainText("sha256:abc");
    expect(inspected).toBe("docker.io/library/nginx:1.27");
    await detail.getByRole("button", { name: "Import", exact: true }).click();
    await expect.poll(() => imported).toEqual({ reference: "docker.io/library/nginx:1.27" });
  });

  test("a check result on a tall panel form is brought into view above the footer", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 400 });
    const details = Object.fromEntries(["Reference", "Digest", "Architecture", "Operating system", "Size", "Layers", "Created", "Entrypoint"].map((key, index) => [key.toLowerCase().replace(/ /g, ""), `value ${index + 1}`]));
    await openDashboard(page, {
      hash: "#/images",
      routes: {
        "/api/images": [],
        "/api/kernels": [],
        "/api/oci/inspect": (route) => route.fulfill({ json: details }),
      },
    });
    await page.getByRole("button", { name: "Import OCI image", exact: true }).click();
    const detail = page.locator("#detail-panel");
    await detail.getByLabel("OCI reference", { exact: true }).fill("nginx:1.27");
    const body = detail.locator(".detail-content");
    await body.evaluate((el) => { el.scrollTop = 0; });
    await detail.getByRole("button", { name: "Inspect image", exact: true }).click();
    const result = detail.locator("#form-result");
    await expect(result).toContainText("value 1");
    await expect(result).toBeInViewport();
    await expect(detail.locator(".form-footer")).toBeInViewport();
    // The whole result is above the footer bar, not under it.
    const boxes = await page.evaluate(() => ({
      resultBottom: document.getElementById("form-result")!.getBoundingClientRect().bottom,
      footerTop: document.querySelector("#editor .form-footer")!.getBoundingClientRect().top,
      scrolls: document.querySelector("#detail-panel .detail-content")!.scrollHeight > document.querySelector("#detail-panel .detail-content")!.clientHeight,
    }));
    expect(boxes.scrolls).toBe(true);
    expect(boxes.resultBottom).toBeLessThanOrEqual(boxes.footerTop + 1);
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
      await page.getByRole("cell", { name: /^production-east(\s|$)/ }).click();
      await expect(panel(page, "MicroNetwork detail")).toBeVisible();
      expect(await fits()).toBe(true);

      await page.getByRole("link", { name: "MicroService", exact: true }).click();
      await page.getByRole("button", { name: "Open orders-api", exact: true }).click();
      await expect(panel(page, "MicroService detail").getByRole("heading", { name: "orders-api" })).toBeVisible();
      expect(await page.locator(".service-list").evaluate(el => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    });
  }
});
