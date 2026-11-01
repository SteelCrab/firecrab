import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("VM pause and resume preserve the instance, and managed Queue VMs stay outside lifecycle controls", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    1,
  );
  const vm = fixture.state.vms[0];
  fixture.state.vms.push({
    ...structuredClone(vm),
    id: "engine",
    name: "managed-queue",
    purpose: "queue",
  });
  fixture.handlers.set("POST /api/vms/" + vm.id + "/pause", ({ send }) => {
    vm.state = "paused";
    send(200, vm);
  });
  fixture.handlers.set("POST /api/vms/" + vm.id + "/resume", ({ send }) => {
    vm.state = "running";
    send(200, vm);
  });
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  try {
    await page.goto(`${fixture.url}/#/vms?vm=${vm.id}`);
    const panel = page.getByRole("complementary", {
      name: "MicroVM detail",
      exact: true,
    });
    const managedRow = page.getByRole("row").filter({
      has: page.getByRole("button", { name: "managed-queue", exact: true }),
    });
    await expect(managedRow).toBeVisible();
    await expect(
      managedRow.getByRole("button", { name: /^(Start|Stop|Pause|Resume)$/ }),
    ).toHaveCount(0);
    await panel.getByRole("button", { name: "Pause", exact: true }).click();
    await expect(panel.getByText("Paused", { exact: true })).toBeVisible();
    await expect(
      panel.getByRole("button", { name: "Delete", exact: true }),
    ).toHaveCount(0);
    await panel.getByRole("button", { name: "Resume", exact: true }).click();
    await expect(panel.getByText("Running", { exact: true })).toBeVisible();
    assert.deepEqual(
      fixture.requests.filter((r) => r.method === "POST").map((r) => r.path),
      ["/api/vms/" + vm.id + "/pause", "/api/vms/" + vm.id + "/resume"],
    );
    assert.deepEqual(errors, []);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("invalid host metrics do not crash a working dashboard", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    1,
  );
  fixture.handlers.set("GET /api/host", ({ send }) =>
    send(200, { config: null }),
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  try {
    await page.goto(`${fixture.url}/#/vms`);
    await expect(
      page.getByRole("button", {
        name: fixture.state.vms[0].name,
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("region", { name: "Host status" }),
    ).toContainText("—");
    assert.deepEqual(errors, []);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("stopped VM security groups stay within their network and preserve a rejected draft", async () => {
  const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      1,
    ),
    vm = fixture.state.vms[0];
  vm.state = "stopped";
  fixture.state.securityGroups = [
    {
      id: "group-1",
      name: "App policy",
      microNetworkId: vm.microNetworkId,
      rules: [],
    },
    {
      id: "group-other",
      name: "Other network",
      microNetworkId: "another-network",
      rules: [],
    },
  ];
  let attempts = 0;
  fixture.handlers.set("PUT /api/vms/" + vm.id, ({ body, send }) => {
    if (++attempts === 1)
      send(400, { error: { message: "Policy changed; inspect and retry." } });
    else {
      Object.assign(vm, body);
      send(200, vm);
    }
  });
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  try {
    await page.goto(`${fixture.url}/#/vms?vm=${vm.id}`);
    const panel = page.getByRole("complementary", {
      name: "MicroVM detail",
      exact: true,
    });
    await panel.getByRole("tab", { name: "Network", exact: true }).click();
    await panel.getByRole("button", { name: "Edit security group" }).click();
    const select = panel.getByRole("combobox", {
      name: "Security group",
      exact: true,
    });
    await expect(select.locator('option[value="group-other"]')).toHaveCount(0);
    await select.selectOption("group-1");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(
      panel.getByText("Policy changed; inspect and retry.", { exact: true }),
    ).toBeVisible();
    await expect(select).toHaveValue("group-1");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(panel.getByText("App policy", { exact: true })).toBeVisible();
    await panel.getByRole("button", { name: "Edit security group" }).click();
    await panel
      .getByRole("combobox", { name: "Security group", exact: true })
      .selectOption("");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => attempts).toBe(3);
    const writes = fixture.requests.filter(
      (r) => r.method === "PUT" && r.path === "/api/vms/" + vm.id,
    );
    assert.equal(writes[1].body.securityGroupId, "group-1");
    assert.equal(writes[2].body.securityGroupId, null);
    assert.equal("env" in writes[2].body, false);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
