import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

const image = {
  alias: "nginx-1.28",
  version: "1.28",
  installed: true,
  description: "",
  hasGuestService: true,
  minDiskGb: 1,
  rootfsSizeBytes: 263192576,
  kernelVersion: "7.2.13",
  kernelImage: "Image-7.2.13-aarch64",
  kernelSha256: "a".repeat(64),
  rootfsSha256: "b".repeat(64),
  packageStaged: false,
  packageUrl: `https://registry.firecrab.dev/${"long-image-name-".repeat(6)}nginx-1.28.tar.zst`,
};

async function setup(t) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  fixture.state.images = [{ ...image }];
  fixture.state.kernels = [
    { version: "7.2.13", installed: true, architecture: "aarch64" },
    { version: "7.2.12", installed: true, architecture: "aarch64" },
  ];
  return { fixture, page };
}

for (const width of [1280, 1600])
  test(`image status cells stay aligned when selection opens the detail panel at ${width}px`, async (t) => {
    const { fixture, page } = await setup(t);
    fixture.state.images.push({
      ...image,
      alias: "nginx-1.27",
      version: "1.27",
    });
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${fixture.url}/#/images`);
    await expect(page.locator("table.image-table tbody tr")).toHaveCount(2);
    for (const alias of [image.alias, "nginx-1.27"]) {
      await page.getByRole("cell", { name: alias, exact: true }).click();
      await expect(
        page.getByRole("complementary", { name: "Image detail" }),
      ).toBeVisible();
      const rows = await page
        .locator("table.image-table tbody tr")
        .evaluateAll((elements) =>
          elements.map((row) => {
            const rect = row.getBoundingClientRect();
            const status = row.querySelector(".state-cell");
            const cell = status.getBoundingClientRect();
            const badge = status
              .querySelector(".state-badge")
              .getBoundingClientRect();
            const heading = row
              .closest("table")
              .querySelector("thead th:last-child")
              .getBoundingClientRect();
            return {
              top: rect.top,
              height: rect.height,
              cellTop: cell.top,
              cellHeight: cell.height,
              cellLeft: cell.left,
              headingLeft: heading.left,
              badgeCenter: badge.top + badge.height / 2,
              backgrounds: [...row.cells].map(
                (cell) => getComputedStyle(cell).backgroundColor,
              ),
            };
          }),
        );
      for (const row of rows) {
        assert(
          Math.abs(row.cellTop - row.top) <= 1,
          "status cell must start at the row top",
        );
        assert(
          Math.abs(row.cellHeight - row.height) <= 1,
          "status cell must fill the row height",
        );
        assert(
          Math.abs(row.cellLeft - row.headingLeft) <= 1,
          "status cell must align with its column heading",
        );
        assert(
          Math.abs(row.badgeCenter - row.top - row.height / 2) <= 1,
          "status text must be vertically centered",
        );
        assert.equal(
          new Set(row.backgrounds).size,
          1,
          "selection background must cover every cell",
        );
      }
    }
  });

for (const width of [390, 1280])
  test(`image details use one delete action and readable sections at ${width}px`, async (t) => {
    const { fixture, page } = await setup(t);
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${fixture.url}/#/images`);
    await page.getByRole("cell", { name: image.alias, exact: true }).click();
    const detail = page.getByRole("complementary", { name: "Image detail" });
    await expect(
      detail.getByRole("heading", { name: image.alias, exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Delete image", exact: true }),
    ).toHaveCount(1);
    await expect(
      detail.getByRole("button", { name: "Delete image", exact: true }),
    ).toBeEnabled();
    await expect(detail.getByRole("button", { name: /download/i })).toHaveCount(
      0,
    );
    const overview = detail.getByRole("region", { name: "Overview" });
    await expect(overview).toContainText("251 MiB");
    await expect(overview).toContainText("1 GiB");
    await expect(
      detail.getByText(image.kernelSha256, { exact: true }),
    ).toBeHidden();
    await expect(
      detail.getByText(image.packageUrl, { exact: true }),
    ).toBeHidden();
    await detail.getByText("Technical details", { exact: true }).click();
    await expect(
      detail.getByText(image.kernelSha256, { exact: true }),
    ).toBeVisible();
    await expect(
      detail.getByText(image.packageUrl, { exact: true }),
    ).toHaveCount(0);
    const box = await detail.boundingBox();
    assert(box.x >= 0 && box.x + box.width <= width + 1);
    assert(
      await detail.evaluate((el) => el.scrollWidth <= el.clientWidth),
      "detail has horizontal overflow",
    );
    const actions = detail.getByRole("button", {
      name: /Delete image|Close detail/,
    });
    const rects = await actions.evaluateAll((elements) =>
      elements.map((el) => {
        const r = el.getBoundingClientRect();
        return { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
      }),
    );
    assert.equal(rects.length, 2);
    const [a, b] = rects;
    assert(
      a.right <= b.left ||
        b.right <= a.left ||
        a.bottom <= b.top ||
        b.bottom <= a.top,
      "detail actions overlap",
    );
  });

test("installed images without an archive explain the missing file and hide archive management", async (t) => {
  const { fixture, page } = await setup(t);
  await page.goto(`${fixture.url}/#/images`);
  await page.getByRole("cell", { name: image.alias, exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Image detail" });
  await detail.getByText("Technical details", { exact: true }).click();
  await expect(
    detail.getByText("Installation file", { exact: true }),
  ).toBeVisible();
  await expect(detail.getByText("Not stored", { exact: true })).toBeVisible();
  await expect(detail).toContainText("The installed image can still be used.");
  await expect(detail.locator(".package-management")).toHaveCount(0);
  await expect(detail.getByText("Not downloaded", { exact: true })).toHaveCount(
    0,
  );
  await expect(detail.getByText(image.packageUrl, { exact: true })).toHaveCount(
    0,
  );
  assert.equal(fixture.requests.filter((r) => r.method !== "GET").length, 0);
});

test("removing a stored installation file retains the installed image and its VM", async (t) => {
  const { fixture, page } = await setup(t);
  fixture.state.images[0].packageStaged = true;
  fixture.state.vms = [
    vm(1, { template: image.alias, name: "web-server", state: "running" }),
  ];
  for (const kind of ["package", "install"]) {
    fixture.handlers.set(`GET /api/images/${image.alias}/${kind}`, ({ send }) =>
      send(200, { alias: image.alias, status: "idle", log: "" }),
    );
  }
  fixture.handlers.set(
    `DELETE /api/images/${image.alias}/package`,
    ({ send }) => {
      fixture.state.images[0].packageStaged = false;
      send(204);
    },
  );
  await page.goto(`${fixture.url}/#/images`);
  await page.getByRole("cell", { name: image.alias, exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Image detail" });
  await detail.getByText("Technical details", { exact: true }).click();
  await expect(detail.getByText("Stored", { exact: true })).toBeVisible();
  await detail.locator(".package-management > summary").click();
  await expect(
    detail.getByRole("button", {
      name: "Download installation file",
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    detail.getByRole("button", { name: "Delete image", exact: true }),
  ).toBeDisabled();
  page.once("dialog", (dialog) => dialog.accept());
  await detail
    .getByRole("button", { name: "Delete installation file", exact: true })
    .click();
  await expect(detail.locator(".package-management")).toHaveCount(0);
  await expect(detail.getByText("Not stored", { exact: true })).toBeVisible();
  await expect(detail.getByRole("link", { name: "web-server" })).toBeVisible();
  await expect(
    page.getByRole("row", { name: /nginx-1\.28.*Installed/ }),
  ).toBeVisible();
  assert.deepEqual(
    fixture.requests.filter((r) => r.method === "DELETE").map((r) => r.path),
    [`/api/images/${image.alias}/package`],
  );
  assert.equal(fixture.state.vms[0].state, "running");
});

test("image delete sends one delete request and kernel change retains the selected version", async (t) => {
  const { fixture, page } = await setup(t);
  fixture.handlers.set(
    "PUT /api/images/nginx-1.28/kernel",
    ({ body, send }) => {
      fixture.state.images[0].kernelVersion = body.kernelVersion;
      send(200, fixture.state.images[0]);
    },
  );
  fixture.handlers.set("DELETE /api/images/nginx-1.28", ({ send }) => {
    fixture.state.images = [];
    send(204);
  });
  await page.goto(`${fixture.url}/#/images`);
  await page.getByRole("cell", { name: image.alias, exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Image detail" });
  await detail.getByText("Change kernel", { exact: true }).click();
  await detail
    .getByLabel("Installed kernel", { exact: true })
    .selectOption("7.2.12");
  await detail
    .getByRole("button", { name: "Apply kernel", exact: true })
    .click();
  await expect
    .poll(
      () =>
        fixture.requests.filter(
          (r) => r.method === "PUT" && r.path.endsWith("/kernel"),
        ).length,
    )
    .toBe(1);
  assert.deepEqual(
    fixture.requests.find(
      (r) => r.method === "PUT" && r.path.endsWith("/kernel"),
    ).body,
    { kernelVersion: "7.2.12" },
  );
  await expect(detail.getByRole("region", { name: "Overview" })).toContainText(
    "7.2.12",
  );
  page.once("dialog", (dialog) => dialog.accept());
  await detail
    .getByRole("button", { name: "Delete image", exact: true })
    .click();
  await expect(
    page.getByRole("cell", { name: image.alias, exact: true }),
  ).toHaveCount(0);
  assert.equal(
    fixture.requests.filter(
      (r) => r.method === "DELETE" && r.path === "/api/images/nginx-1.28",
    ).length,
    1,
  );
});

test("an image used by a VM disables deletion and kernel changes", async (t) => {
  const { fixture, page } = await setup(t);
  fixture.state.vms = [
    vm(1, { template: image.alias, name: "web-server", state: "running" }),
  ];
  await page.goto(`${fixture.url}/#/images`);
  await page.getByRole("cell", { name: image.alias, exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Image detail" });
  await expect(detail.getByRole("link", { name: "web-server" })).toBeVisible();
  await expect(
    detail.getByRole("button", { name: "Delete image", exact: true }),
  ).toBeDisabled();
  await detail.getByText("Change kernel", { exact: true }).click();
  await expect(
    detail.getByLabel("Installed kernel", { exact: true }),
  ).toBeDisabled();
  await expect(
    detail.getByRole("button", { name: "Apply kernel", exact: true }),
  ).toBeDisabled();
});
