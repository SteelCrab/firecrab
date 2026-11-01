import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

test("VM usage identifies its source and uses comparable percentage charts", async (t) => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  const start = Date.UTC(2026, 9, 30, 9, 0, 0);
  fixture.state.vms = [
    vm(0, {
      cpu: 2,
      ram: 4096,
      cpuUsagePercent: 0.2,
      memoryUsedMib: 133,
      memoryTotalMib: 1024,
      memoryUsedPercent: 13,
      usageHistory: [0.1, 0.2, 0.3].map((value, index) => ({
        atMs: start + index * 3000,
        cpuUsagePercent: value,
        memoryUsedMib: 133,
        memoryTotalMib: 1024,
        memoryUsedPercent: 13,
      })),
    }),
  ];
  for (const width of [390, 1280]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`${fixture.url}/#/vms`);
    await page.getByRole("button", { name: "vm-00000", exact: true }).click();
    const detail = page.getByRole("complementary", { name: "MicroVM detail" });
    const usage = detail.getByRole("region", {
      name: "This VM: CPU and memory",
    });
    await expect(usage).toContainText("Measured inside this VM.");
    const cpu = usage.getByRole("region", { name: "VM CPU usage" });
    await expect
      .poll(
        () =>
          usage.evaluate((el) => {
            const card = el
              .querySelector(".vm-usage-card")
              .getBoundingClientRect();
            const description = el
              .querySelector(".vm-usage-description")
              .getBoundingClientRect();
            return (
              card.top >= description.bottom &&
              card.width > el.getBoundingClientRect().width * 0.8
            );
          }),
        {
          message:
            "description and charts must stay ordered and fill the panel",
        },
      )
      .toBe(true);
    await expect(cpu).toContainText("0.2%");
    await expect(cpu).toContainText("2 vCPU(s)");
    await expect(
      cpu.getByRole("img", { name: "CPU usage history, 0 to 100 percent" }),
    ).toBeVisible();
    const path = await cpu.locator(".sparkline-line").getAttribute("d");
    assert(
      [...path.matchAll(/[ML][\d.]+ ([\d.]+)/g)].every(
        (match) => Number(match[1]) > 50,
      ),
      "low CPU values must remain near zero on the 0–100% chart",
    );
    const memory = usage.getByRole("region", { name: "VM memory usage" });
    await expect(memory).toContainText("13%");
    await expect(memory).toContainText("133 MiB / 1 GiB");
    await expect(memory).toContainText("used / guest total");
    await expect(memory).not.toContainText("4 GiB");
    await expect(
      memory.getByRole("img", {
        name: "Memory usage history, 0 to 100 percent",
      }),
    ).toBeVisible();
    await expect
      .poll(() => detail.evaluate((el) => el.scrollWidth <= el.clientWidth), {
        message: "usage cards overflow the panel",
      })
      .toBe(true);
  }
});

test("memory measurements remain visible while CPU measurements are pending", async (t) => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  fixture.state.vms = [
    vm(0, {
      cpuUsagePercent: null,
      memoryUsedMib: 128,
      memoryTotalMib: 1024,
      memoryUsedPercent: 12.5,
      usageHistory: [],
    }),
  ];
  await page.goto(`${fixture.url}/#/vms`);
  await page.getByRole("button", { name: "vm-00000", exact: true }).click();
  const memory = page.getByRole("region", { name: "VM memory usage" });
  await expect(memory).toContainText("128 MiB / 1 GiB");
  await expect(memory).toContainText("Collecting usage history…");
  await expect(
    page
      .getByRole("region", { name: "VM CPU usage" })
      .locator(".vm-usage-card-value"),
  ).toHaveText("—");
});
