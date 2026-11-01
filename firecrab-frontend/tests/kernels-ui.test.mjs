import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("kernel install resumes an existing job and protects kernels referenced by images", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const kernel = {
    version: "7.2.12",
    architecture: "aarch64",
    installed: false,
    inUse: false,
  };
  fixture.state.kernels = [kernel];
  fixture.state.images = [
    { alias: "nginx-1.27", installed: true, kernelVersion: "7.2.12" },
  ];
  let status = "idle";
  fixture.handlers.set("GET /api/kernels/7.2.12/install", ({ send }) =>
    send(200, {
      version: "7.2.12",
      status,
      log: status === "idle" ? "" : "building Linux 7.2.12",
    }),
  );
  fixture.handlers.set("POST /api/kernels/7.2.12/install", ({ send }) => {
    status = "running";
    send(409, {
      error: {
        code: "kernel_install_in_progress",
        message: "a kernel install is already running",
      },
    });
  });
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    await page.getByRole("button", { name: "7.2.12", exact: true }).click();
    await page.getByRole("button", { name: "Build kernel" }).click();
    await expect(
      page.getByRole("button", { name: "Build kernel" }),
    ).toBeDisabled();
    await expect(page.getByRole("progressbar")).toHaveCount(1);
    await expect(page.locator("body")).not.toContainText("already running");
    await expect(
      page.getByRole("button", { name: "Delete", exact: true }),
    ).toHaveCount(0);
    kernel.installed = true;
    kernel.inUse = true;
    status = "succeeded";
    await expect(
      page.getByRole("button", { name: "Delete", exact: true }),
    ).toBeDisabled({ timeout: 10000 });
    await expect(page.getByRole("progressbar")).toHaveCount(0);
    await expect(
      page.getByRole("row", { name: /7\.2\.12 Installed/ }),
    ).toBeVisible();
    await expect(page.locator(".kernel-react-images")).toHaveText("nginx-1.27");
    await expect(page.locator(".kernel-react-images svg")).toHaveCount(1);
    kernel.inUse = false;
    await expect(
      page.getByRole("button", { name: "Delete", exact: true }),
    ).toBeEnabled({ timeout: 10000 });
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("completed kernel progress stays hidden after reload while failed jobs remain visible", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.kernels = ["7.2.13", "7.2.12", "7.2.11"].map(
    (version, index) => ({
      version,
      architecture: "aarch64",
      installed: index < 2,
      inUse: false,
    }),
  );
  for (const kernel of fixture.state.kernels) {
    fixture.handlers.set(
      `GET /api/kernels/${kernel.version}/install`,
      ({ send }) =>
        send(200, {
          version: kernel.version,
          status: kernel.installed ? "succeeded" : "failed",
          log: kernel.installed ? "verified and cached" : "kernel build failed",
        }),
    );
  }
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    for (const reload of [false, true]) {
      if (reload) await page.reload();
      await expect(page.getByRole("progressbar")).toHaveCount(1);
      await expect(
        page.locator(".panel").filter({ has: page.getByRole("progressbar") }),
      ).toContainText("7.2.11");
      await expect(page.getByRole("row", { name: /Installed/ })).toHaveCount(2);
      for (const version of ["7.2.13", "7.2.12"]) {
        await expect(
          page.getByRole("button", { name: version, exact: true }),
        ).toHaveCount(1);
      }
    }
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
