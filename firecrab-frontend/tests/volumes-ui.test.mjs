import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("volume creation offers one default storage and posts the selected capacity", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.handlers.set("GET /api/storage", ({ send }) =>
    send(200, [
      { id: "default", name: "default" },
      { id: "extra", name: "Extra" },
    ]),
  );
  fixture.handlers.set("POST /api/microblockstorage", ({ body, send }) => {
    fixture.state.volumes.push({
      ...body,
      id: "v1",
      state: "available",
      storageRoot: body.storageRoot || "default",
      attachedVmId: null,
      mountPath: null,
    });
    send(201, fixture.state.volumes[0]);
  });
  try {
    await page.goto(`${fixture.url}/#/volumes`);
    await page.getByRole("button", { name: "Create volume" }).click();
    await expect(page.getByLabel("Storage").locator("option")).toHaveCount(2);
    await page.getByLabel("Name", { exact: true }).fill("logs");
    await page.getByLabel("Size (GiB)").fill("4");
    await page
      .locator("#editor")
      .getByRole("button", { name: "Create volume", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: "logs · 4 GiB" }),
    ).toBeVisible();
    assert.deepEqual(
      fixture.requests.find(
        (request) =>
          request.method === "POST" &&
          request.path === "/api/microblockstorage",
      ).body,
      { name: "logs", sizeGib: 4, storageRoot: "default" },
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("stopped MicroVM attaches and detaches an available volume", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    1,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.vms[0].state = "stopped";
  fixture.state.volumes = [
    {
      id: "v1",
      name: "logs",
      sizeGib: 4,
      state: "available",
      storageRoot: "default",
      attachedVmId: null,
      mountPath: null,
    },
  ];
  fixture.handlers.set(
    "POST /api/microblockstorage/v1/attach",
    ({ body, send }) => {
      Object.assign(fixture.state.volumes[0], {
        state: "attached",
        attachedVmId: body.vmId,
        mountPath: body.mountPath,
      });
      send(200, fixture.state.volumes[0]);
    },
  );
  fixture.handlers.set("POST /api/microblockstorage/v1/detach", ({ send }) => {
    Object.assign(fixture.state.volumes[0], {
      state: "available",
      attachedVmId: null,
      mountPath: null,
    });
    send(200, fixture.state.volumes[0]);
  });
  try {
    await page.goto(`${fixture.url}/#/vms`);
    await page.getByRole("button", { name: "vm-00000", exact: true }).click();
    await page.getByRole("tab", { name: "Volumes" }).click();
    await page
      .getByRole("button", { name: "Attach volume", exact: true })
      .click();
    await page.getByLabel("Available volume").selectOption("v1");
    await page.getByLabel("Mount path").fill("/data");
    await page.getByRole("button", { name: "Attach volume" }).click();
    await expect(page.getByRole("button", { name: "Detach" })).toBeEnabled();
    assert.deepEqual(
      fixture.requests.find((request) => request.path.endsWith("/v1/attach"))
        .body,
      { vmId: fixture.state.vms[0].id, mountPath: "/data" },
    );
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Detach" }).click();
    await expect(
      page.getByRole("button", { name: "Attach volume", exact: true }),
    ).toBeVisible();
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
