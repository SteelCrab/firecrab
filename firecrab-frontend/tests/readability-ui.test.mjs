import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("dashboard remains readable and fits desktop and phone widths", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  try {
    for (const width of [320, 390, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`${fixture.url}/#/images`);
      await expect(
        page.getByRole("heading", { name: "Images", exact: true }),
      ).toBeVisible();
      await expect(page.locator(".panel-intro")).toBeAttached();
      const sizes = await page.evaluate(() => ({
        body: parseFloat(getComputedStyle(document.body).fontSize),
        intro: parseFloat(
          getComputedStyle(document.querySelector(".panel-intro")).fontSize,
        ),
        scroll: document.documentElement.scrollWidth,
        width: innerWidth,
      }));
      assert(sizes.body >= 17 && sizes.intro >= 16);
      assert(
        sizes.scroll <= sizes.width,
        `horizontal overflow at ${width}px: ${sizes.scroll}px`,
      );
    }
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("the VM list keeps instance names visible beside its detail panel", async () => {
  const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      1,
    ),
    browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  try {
    for (const width of [1280, 1440, 1920]) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`${fixture.url}/#/vms?vm=${fixture.state.vms[0].id}`);
      const name = page.getByRole("button", {
        name: fixture.state.vms[0].name,
        exact: true,
      });
      await expect(name).toBeVisible();
      const bounds = await name.boundingBox(),
        list = await page.locator(".vm-list-scroll").boundingBox();
      assert(
        bounds.width > 0 &&
          bounds.x >= list.x &&
          bounds.x + bounds.width <= list.x + list.width,
      );
      assert.equal(
        await page
          .locator(".vm-list-scroll")
          .evaluate((el) => el.scrollWidth > el.clientWidth),
        false,
      );
      assert(
        (await name.evaluate((el) =>
          parseFloat(getComputedStyle(el).fontSize),
        )) >= 18,
      );
    }
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("creation drawers keep the list clear and the header spans both columns", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 900 },
  });
  try {
    for (const [view, button] of [
      ["shells", "New Shell"],
      ["volumes", "Create volume"],
    ]) {
      await page.goto(`${fixture.url}/#/${view}`);
      await expect(page.getByRole("complementary")).toHaveCount(0);
      await expect(page.locator("#content form")).toHaveCount(0);
      await page
        .locator(".screen-header")
        .getByRole("button", { name: button, exact: true })
        .click();
      const panel = page.getByRole("complementary");
      await expect(
        panel.getByRole("textbox", { name: "Name", exact: true }),
      ).toBeVisible();
      const header = await page.locator(".screen-header").boundingBox();
      const drawer = await panel.boundingBox();
      assert(header.x < drawer.x);
      assert(Math.abs(header.x + header.width - drawer.x - drawer.width) <= 1);
      assert(drawer.y >= header.y + header.height);
      await expect(page.locator("#content form")).toHaveCount(0);
    }
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
