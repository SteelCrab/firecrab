import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("installation files can still be downloaded and installed separately", async (t) => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  const image = {
    alias: "custom-image",
    version: "v2",
    installed: false,
    packageStaged: false,
    packageUrl: "https://example.test/custom-image.tar.zst",
    minDiskGb: 2,
  };
  fixture.state.images = [image];
  for (const kind of ["package", "install"]) {
    fixture.handlers.set(`GET /api/images/${image.alias}/${kind}`, ({ send }) =>
      send(200, { alias: image.alias, status: "idle", log: "" }),
    );
    fixture.handlers.set(
      `POST /api/images/${image.alias}/${kind}`,
      ({ send }) => {
        if (kind === "package") image.packageStaged = true;
        else image.installed = true;
        send(202, { alias: image.alias, status: "succeeded", log: "" });
      },
    );
  }
  await page.goto(`${fixture.url}/#/images`);
  await page.getByRole("cell", { name: image.alias, exact: true }).click();
  const detail = page.getByRole("complementary", { name: "Image detail" });
  await expect(
    detail.getByRole("button", { name: "Install", exact: true }),
  ).toBeDisabled();
  await detail
    .getByRole("button", { name: "Download installation file", exact: true })
    .click();
  await expect(
    detail.getByRole("button", {
      name: "Download installation file",
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    detail.getByRole("button", { name: "Install", exact: true }),
  ).toBeEnabled();
  await detail.getByRole("button", { name: "Install", exact: true }).click();
  await expect(
    page.getByRole("row", { name: /custom-image.*Installed/ }),
  ).toBeVisible();
  assert.deepEqual(
    fixture.requests.filter((r) => r.method === "POST").map((r) => r.path),
    ["/api/images/custom-image/package", "/api/images/custom-image/install"],
  );
});

test("image packages omit default suggestions but register and install actual packages", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  fixture.state.images = [];
  const packages = [
    { alias: "alpine-3.24.1", version: "v1", downloadable: true },
    { alias: "custom-image", version: "v2", downloadable: true },
  ];
  fixture.handlers.set("GET /api/microregistry", ({ send }) =>
    send(200, { source: "fixture", images: packages }),
  );
  fixture.handlers.set("POST /api/microregistry/register", ({ send }) =>
    send(202, {
      jobId: "job-1",
      alias: "custom-image",
      status: "running",
      log: "",
    }),
  );
  fixture.handlers.set("GET /api/microregistry/jobs/job-1", ({ send }) => {
    fixture.state.images = [
      {
        alias: "custom-image",
        version: "v2",
        installed: false,
        packageStaged: true,
        minDiskGb: 2,
      },
    ];
    send(200, {
      jobId: "job-1",
      alias: "custom-image",
      status: "succeeded",
      log: "",
    });
  });
  fixture.handlers.set("GET /api/images/custom-image/package", ({ send }) =>
    send(200, { alias: "custom-image", status: "idle", log: "" }),
  );
  fixture.handlers.set("GET /api/images/custom-image/install", ({ send }) =>
    send(200, { alias: "custom-image", status: "idle", log: "" }),
  );
  fixture.handlers.set("POST /api/images/custom-image/install", ({ send }) =>
    send(202, { alias: "custom-image", status: "running", log: "" }),
  );
  try {
    await page.goto(`${fixture.url}/#/images`);
    await expect(page.getByText("alpine-3.24.1", { exact: false })).toHaveCount(
      0,
    );
    await page
      .getByRole("button", { name: "Register package", exact: true })
      .click();
    await expect(page.getByText("alpine-3.24.1", { exact: false })).toHaveCount(
      0,
    );
    await expect(
      page.getByRole("heading", { name: "Available packages" }),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "Register", exact: true }).click();
    await expect(page.locator("tbody").getByText("custom-image")).toBeVisible();
    await page.locator("tbody").getByText("custom-image").click();
    await page.getByRole("button", { name: "Install", exact: true }).click();
    assert.deepEqual(
      fixture.requests.find((r) => r.path === "/api/microregistry/register")
        .body,
      { alias: "custom-image", version: "v2" },
    );
    await expect
      .poll(
        () =>
          fixture.requests.filter(
            (r) =>
              r.method === "POST" &&
              r.path === "/api/images/custom-image/install",
          ).length,
      )
      .toBe(1);
    const install = fixture.requests.find(
      (r) =>
        r.method === "POST" && r.path === "/api/images/custom-image/install",
    );
    assert.match(install.headers["x-firecrab-operation-id"], /^[a-f0-9-]{36}$/);
    assert.equal(install.headers["x-firecrab-endpoint-id"], fixture.endpointId);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
