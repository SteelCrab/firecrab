import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("OCI image import uses the side panel and reconnects to an existing import", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.images = ["alpine-3.24.1", "rocky-9.8", "ubuntu-26.04"].map(
    (alias) => ({ alias, version: alias, installed: false, minDiskGb: 0 }),
  );
  let status = "idle";
  fixture.handlers.set("GET /api/oci/inspect", ({ send }) =>
    send(200, {
      alias: "nginx-1.27",
      architecture: "aarch64",
      digest: "sha256:test",
      version: "1.27",
      registry: "registry-1.docker.io",
      repository: "library/nginx",
      singlePlatform: false,
      immutable: false,
    }),
  );
  fixture.handlers.set("GET /api/oci/import/nginx-1.27", ({ send }) =>
    send(200, {
      alias: "nginx-1.27",
      status,
      log: status === "idle" ? "" : "caching image blobs",
    }),
  );
  fixture.handlers.set("POST /api/oci/import", ({ send }) => {
    status = "running";
    send(409, {
      error: {
        code: "import_in_progress",
        message: "an import is already running",
      },
    });
  });
  try {
    await page.goto(`${fixture.url}/#/images`);
    await expect(page.locator("body")).not.toContainText("alpine-3.24.1");
    await expect(page.locator("body")).not.toContainText("rocky-9.8");
    await expect(page.locator("body")).not.toContainText("ubuntu-26.04");
    await page.getByRole("button", { name: "Import OCI image" }).click();
    const panel = page.getByRole("complementary", { name: "Image detail" });
    await panel.getByLabel("Reference", { exact: true }).fill("nginx:1.27");
    await panel.getByRole("button", { name: "Inspect", exact: true }).click();
    await expect(panel).toContainText("nginx-1.27");
    await panel.getByRole("button", { name: "Start Import" }).click();
    await expect(panel).toContainText("Downloading image");
    await expect(
      panel.getByRole("progressbar", { name: "Installation stages" }),
    ).toHaveAttribute("max", "4");
    await expect(panel).not.toContainText("an import is already running");
    await expect(
      panel.getByRole("button", { name: "Importing…" }),
    ).toBeDisabled();
    await page.reload();
    await expect(
      page.getByRole("complementary", { name: "Image detail" }),
    ).toContainText("Downloading image");
    await expect(page.locator("body")).not.toContainText(
      "an import is already running",
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
