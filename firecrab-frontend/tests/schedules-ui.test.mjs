import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

test("schedules hide the VM filter when no ordinary VMs are available", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.vms = [vm(0, { purpose: "queue" }), vm(1, { purpose: "pool" })];
  fixture.state.schedules = [
    {
      id: "s1",
      name: "orphan",
      vmId: "removed",
      action: "start",
      timing: { kind: "cron", expression: "0 0 * * *", timeZone: "UTC" },
      enabled: true,
      lastRun: null,
      nextRunAtMs: null,
      createdAtMs: 0,
      updatedAtMs: 0,
    },
  ];
  try {
    await page.goto(`${fixture.url}/#/schedules?vm=removed`);
    await expect(page.getByText("orphan", { exact: true })).toBeVisible();
    await expect(page.getByLabel("Filter by VM")).toHaveCount(0);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
