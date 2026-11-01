import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("security group creation sends a JSON object with the selected network", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.services = [];
  fixture.handlers.set("POST /api/security-groups", ({ body, send }) => {
    const group = {
      id: "group-1",
      name: body.name,
      microNetworkId: body.microNetworkId,
      rules: [],
    };
    fixture.state.securityGroups.push(group);
    send(201, group);
  });
  try {
    await page.goto(`${fixture.url}/#/security-groups`);
    await page.getByRole("button", { name: "New security group" }).click();
    await page.getByLabel("Name", { exact: true }).fill("web-security");
    await page
      .getByLabel("MicroNetwork", { exact: true })
      .selectOption("network-1");
    await page
      .getByRole("button", { name: "Create security group", exact: true })
      .click();
    await expect(page.getByText("web-security", { exact: true })).toBeVisible();
    assert.deepEqual(
      fixture.requests.find(
        (request) =>
          request.method === "POST" && request.path === "/api/security-groups",
      ).body,
      { name: "web-security", microNetworkId: "network-1" },
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
