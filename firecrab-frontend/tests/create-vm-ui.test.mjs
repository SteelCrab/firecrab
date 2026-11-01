import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

test("new MicroVM submits the selected security group and data volume", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.securityGroups = [
    {
      id: "group-1",
      name: "web-security",
      microNetworkId: "network-1",
      rules: [],
    },
  ];
  fixture.state.volumes = [
    {
      id: "vol-1",
      name: "data-volume",
      state: "available",
      attachedVmId: null,
      sizeGib: 1,
      storageRoot: "default",
    },
  ];
  fixture.handlers.set("POST /api/vms", ({ send }) => {
    const created = vm(1, {
      id: "vm-created",
      name: "example",
      state: "created",
    });
    fixture.state.vms.push(created);
    send(201, created);
  });
  try {
    await page.goto(`${fixture.url}/#/vms/new`);
    await page.locator("#vm-name").fill("example");
    await page
      .getByLabel("Security group", { exact: true })
      .selectOption("group-1");
    await page.getByText("Advanced options", { exact: true }).click();
    await page.getByLabel("data-volume").check();
    await page.getByLabel("Mount path").fill("/data");
    await page
      .getByRole("button", { name: "Create MicroVM", exact: true })
      .click();
    await expect
      .poll(() =>
        fixture.requests.some(
          (r) => r.path === "/api/vms" && r.method === "POST",
        ),
      )
      .toBe(true);
    const body = fixture.requests.find(
      (r) => r.path === "/api/vms" && r.method === "POST",
    ).body;
    assert.equal(body.securityGroupId, "group-1");
    assert.deepEqual(body.blockStorages, [
      { volumeId: "vol-1", mountPath: "/data" },
    ]);
    await expect(page).toHaveURL(/#\/vms$/);
    await expect(
      page.getByRole("button", { name: "example", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".banner.info")).toHaveCount(0);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
