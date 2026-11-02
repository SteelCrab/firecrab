import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

async function setup(t) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  page.setDefaultTimeout(5000);
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  t.after(async () => {
    await browser.close();
    await fixture.close();
  });
  await page.goto(`${fixture.url}/#/images`);
  return { fixture, page, image: fixture.state.images[0] };
}
async function remove(page, alias) {
  await page.getByRole("cell", { name: alias, exact: true }).click();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Delete image", exact: true }).click();
}
const pending = (page) =>
  page.evaluate(() =>
    Object.entries(sessionStorage).filter(([key]) =>
      key.startsWith("firecrab.native-mutation:"),
    ),
  );
test("lost image delete acknowledgement observes its original completion without a second deletion", async (t) => {
  const { fixture, page, image } = await setup(t);
  let effects = 0;
  const ids = [];
  fixture.handlers.set(`DELETE /api/images/${image.alias}`, ({ req, res }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    if (!fixture.nativeOperations.has(id)) effects++;
    assert.match(id, /^[a-f0-9-]{36}$/);
    assert.equal(req.headers["x-firecrab-endpoint-id"], fixture.endpointId);
    fixture.state.images = [];
    fixture.nativeOperations.set(id, {
      operationId: id,
      action: "delete_image",
      state: "succeeded",
      ownership: {
        endpointId: fixture.endpointId,
        resource: { kind: "image", id: image.alias },
      },
    });
    res.destroy();
  });
  await remove(page, image.alias);
  await expect(
    page.getByRole("cell", { name: image.alias, exact: true }),
  ).toHaveCount(0);
  assert.equal(effects, 1);
  assert.equal(new Set(ids).size, 1);
  assert.deepEqual(await pending(page), []);
});
test("a mismatched image receipt preserves the original identity across reload and retry", async (t) => {
  const { fixture, page, image } = await setup(t);
  const ids = [];
  let retryEnabled = false;
  fixture.handlers.set(
    `DELETE /api/images/${image.alias}`,
    ({ req, res, send }) => {
      const id = req.headers["x-firecrab-operation-id"];
      ids.push(id);
      if (!retryEnabled) {
        fixture.nativeOperations.set(id, {
          operationId: id,
          action: "delete_image",
          state: "succeeded",
          ownership: {
            endpointId: "26d90062-dc1d-49db-98c4-88b4626c632e",
            resource: { kind: "image", id: image.alias },
          },
        });
        res.destroy();
      } else {
        fixture.state.images = [];
        send(204);
      }
    },
  );
  await remove(page, image.alias);
  await expect(page.locator(".field-error")).toContainText(
    "Original operation identity changed",
  );
  const retained = await pending(page);
  assert.equal(retained.length, 1);
  assert.deepEqual(Object.keys(JSON.parse(retained[0][1])).sort(), [
    "endpointId",
    "operationId",
  ]);
  await page.reload();
  retryEnabled = true;
  await remove(page, image.alias);
  await expect(
    page.getByRole("cell", { name: image.alias, exact: true }),
  ).toHaveCount(0);
  assert(ids.length >= 2);
  assert.equal(new Set(ids).size, 1);
  assert.deepEqual(await pending(page), []);
});
