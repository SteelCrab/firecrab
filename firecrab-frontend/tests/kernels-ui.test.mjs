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
  let posts = 0;
  fixture.handlers.set("GET /api/kernels/7.2.12/install", ({ send }) =>
    send(200, {
      version: "7.2.12",
      status,
      log: status === "idle" ? "" : "building Linux 7.2.12",
    }),
  );
  fixture.handlers.set("POST /api/kernels/7.2.12/install", ({ send }) => {
    posts++;
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
      page.getByRole("button", { name: "Build kernel", exact: true }),
    ).toHaveCount(0);
    await expect(page.getByRole("progressbar")).toHaveCount(1);
    await expect(page.locator("body")).not.toContainText("already running");
    await expect(
      page.getByRole("button", { name: "Delete", exact: true }),
    ).toHaveCount(0);
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Build kernel", exact: true }),
    ).toHaveCount(0);
    await expect(page.getByRole("progressbar")).toHaveCount(1);
    expect(posts).toBe(1);
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
      await expect(
        page.getByRole("heading", { name: "Installation failed", exact: true }),
      ).toBeVisible();
      await expect(page.getByRole("progressbar")).toHaveCount(0);
      await expect(
        page.getByRole("complementary", { name: "Kernel detail", exact: true }),
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

test("the right panel follows real download, queue, build and verification milestones", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1600, height: 1000 },
  });
  const kernel = {
    version: "7.3.1",
    architecture: "aarch64",
    installed: false,
    inUse: false,
  };
  fixture.state.kernels = [kernel];
  const job = {
    version: kernel.version,
    status: "running",
    startedAtMs: Date.now(),
    log: "[01:00:00] kernel source build started\n[01:00:00] resolving Linux 7.3.1 from kernel.org source",
  };
  let unavailable = false;
  fixture.handlers.set("GET /api/kernels/7.3.1/install", ({ send }) =>
    send(
      unavailable ? 503 : 200,
      unavailable ? { error: { message: "temporarily unavailable" } } : job,
    ),
  );
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    const panel = page.getByRole("complementary", {
      name: "Kernel detail",
      exact: true,
    });
    const steps = panel.getByRole("list", { name: "Installation steps" });
    const currentStep = steps.locator('[aria-current="step"]');
    const progress = panel.getByRole("progressbar");
    await expect(panel).toBeVisible();
    const rowBox = await page
      .getByRole("row", { name: /7\.3\.1 Preparing/ })
      .boundingBox();
    const panelBox = await panel.boundingBox();
    expect(panelBox.x).toBeGreaterThanOrEqual(rowBox.x + rowBox.width - 1);
    await expect(currentStep).toContainText("Prepare source");
    await expect(progress).not.toHaveAttribute("aria-valuenow");

    job.log +=
      "\n[01:00:01] downloading and verifying https://cdn.kernel.org/linux.tar.xz";
    job.downloadedBytes = 64 * 1024 ** 2;
    job.totalBytes = 160 * 1024 ** 2;
    await expect(currentStep).toContainText("Download source", {
      timeout: 10000,
    });
    await expect(progress).toHaveAttribute("aria-valuenow", "40");
    await expect(panel).toContainText("64.0 MiB / 160.0 MiB");

    job.log += "\n[01:00:02] waiting for the host kernel builder";
    job.downloadedBytes = job.totalBytes;
    await expect(currentStep).toContainText("Wait for builder", {
      timeout: 10000,
    });
    await expect(progress).not.toHaveAttribute("aria-valuenow");
    await expect(steps.locator('[data-state="complete"]')).toHaveCount(2);
    await page.reload();
    await expect(currentStep).toContainText("Wait for builder");
    await expect(panel).toContainText("Kernel builds run one at a time.");

    job.log +=
      "\n[01:00:03] building Linux 7.3.1 for aarch64\n[01:00:04] CC kernel/sched/core.o";
    await expect(currentStep).toContainText("Build kernel", { timeout: 10000 });
    await expect(progress).not.toHaveAttribute("aria-valuenow");
    await expect(steps.locator('[data-state="complete"]')).toHaveCount(3);
    await expect(panel).toContainText("CC kernel/sched/core.o");
    await expect(panel.locator("details")).not.toHaveAttribute("open");
    unavailable = true;
    await expect(panel.getByRole("status")).toContainText("last known status", {
      timeout: 10000,
    });
    await expect(currentStep).toContainText("Build kernel");
    unavailable = false;

    job.log += "\n[01:00:05] verifying built kernel";
    await expect(currentStep).toContainText("Verify and install", {
      timeout: 10000,
    });
    await expect(panel.getByRole("status")).toHaveCount(0);
    await expect(steps.locator('[data-state="complete"]')).toHaveCount(4);
    job.status = "succeeded";
    kernel.installed = true;
    await expect(
      page.getByRole("row", { name: /7\.3\.1 Installed/ }),
    ).toBeVisible({ timeout: 10000 });
    await expect(progress).toHaveCount(0);
    await expect(steps).toHaveCount(0);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("queued jobs keep separate panels and work on a Korean phone layout", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "ko"));
  fixture.state.kernels = ["7.3.1", "7.3"].map((version) => ({
    version,
    architecture: "aarch64",
    installed: false,
    inUse: false,
  }));
  for (const kernel of fixture.state.kernels) {
    fixture.handlers.set(
      `GET /api/kernels/${kernel.version}/install`,
      ({ send }) =>
        send(200, {
          version: kernel.version,
          status: "running",
          downloadedBytes: 160 * 1024 ** 2,
          log:
            kernel.version === "7.3.1"
              ? "building Linux 7.3.1 for aarch64"
              : "waiting for the host kernel builder",
        }),
    );
  }
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    const panel = page.getByRole("complementary", {
      name: "커널 상세",
      exact: true,
    });
    await expect(
      panel.getByRole("heading", { name: "7.3.1", exact: true }),
    ).toBeVisible();
    await panel.getByRole("button", { name: "상세 닫기", exact: true }).click();
    await expect(panel).toHaveCount(0);
    await page.getByRole("button", { name: "7.3", exact: true }).click();
    await expect(
      panel.getByRole("heading", { name: "7.3", exact: true }),
    ).toBeVisible();
    await expect(panel).toContainText("Linux 7.3.1이 빌드 중이다.");
    await expect(panel.locator('[aria-current="step"]')).toContainText(
      "빌드 대기",
    );
    expect(
      await panel.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
    ).toBe(true);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("kernel release dates sort independently from versions and EOL releases remain installable", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1000 },
    timezoneId: "America/Los_Angeles",
  });
  fixture.state.kernels = [
    { version: "7.3.1", releasedAt: "2026-10-30", endOfLife: false },
    { version: "7.3", releasedAt: "2026-10-18", endOfLife: false },
    { version: "7.2.15", releasedAt: "2026-11-01", endOfLife: true },
    { version: "7.2.13", installed: true },
  ].map((kernel) => ({
    architecture: "aarch64",
    installed: false,
    inUse: false,
    ...kernel,
  }));
  let eolJob = { version: "7.2.15", status: "idle", log: "" };
  for (const kernel of fixture.state.kernels) {
    fixture.handlers.set(
      `GET /api/kernels/${kernel.version}/install`,
      ({ send }) =>
        send(
          200,
          kernel.version === "7.2.15"
            ? eolJob
            : {
                version: kernel.version,
                status: "idle",
                log: "",
              },
        ),
    );
  }
  let installations = 0;
  fixture.handlers.set("POST /api/kernels/7.2.15/install", ({ send }) => {
    installations++;
    eolJob = {
      version: "7.2.15",
      status: "running",
      log: "downloading and verifying linux-7.2.15.tar.xz",
    };
    send(202, eolJob);
  });
  const versions = () =>
    page.locator(".kernel-table tbody .kernel-version").allTextContents();
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    await expect(page.locator(".kernel-table tbody tr")).toHaveCount(4);
    expect(await versions()).toEqual(["7.3.1", "7.3", "7.2.15", "7.2.13"]);
    const eolRow = page.locator(".kernel-table tr").filter({
      has: page.getByRole("button", { name: "7.2.15", exact: true }),
    });
    await expect(eolRow.locator(".kernel-eol")).toHaveText("EOL");
    await expect(eolRow.locator("time")).toHaveAttribute(
      "datetime",
      "2026-11-01",
    );
    await expect(eolRow.locator("time")).toHaveText("2026-11-01");
    await page.getByRole("button", { name: "Released", exact: true }).click();
    await expect(
      page.getByRole("columnheader", { name: "Released" }),
    ).toHaveAttribute("aria-sort", "descending");
    expect(await versions()).toEqual(["7.2.15", "7.3.1", "7.3", "7.2.13"]);
    await page.getByRole("button", { name: "Released", exact: true }).click();
    expect(await versions()).toEqual(["7.3", "7.3.1", "7.2.15", "7.2.13"]);
    await page.getByRole("button", { name: "7.2.15", exact: true }).click();
    await expect(page.locator(".kernel-detail .kernel-eol")).toHaveText("EOL");
    await expect(page.locator(".kernel-detail time")).toHaveText("2026-11-01");
    await page
      .getByRole("button", { name: "Build kernel", exact: true })
      .click();
    await expect(page.getByRole("progressbar")).toHaveCount(1);
    expect(installations).toBe(1);
    await page.setViewportSize({ width: 390, height: 844 });
    await page
      .getByRole("button", { name: "Close detail", exact: true })
      .click();
    await expect(eolRow.locator("time")).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
  } finally {
    await browser.close();
    await fixture.close();
  }
});

test("kernel cancellation stays pending across reload until cleanup finishes and then allows retry", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1000 },
  });
  fixture.state.kernels = [
    {
      version: "7.2.15",
      architecture: "aarch64",
      installed: false,
      inUse: false,
    },
  ];
  let job = {
    version: "7.2.15",
    status: "running",
    cancelling: false,
    log: "building Linux 7.2.15",
    startedAtMs: Date.now() - 5000,
  };
  let cancellations = 0;
  let starts = 0;
  fixture.handlers.set("GET /api/kernels/7.2.15/install", ({ send }) =>
    send(200, job),
  );
  fixture.handlers.set("DELETE /api/kernels/7.2.15/install", ({ send }) => {
    cancellations++;
    job = { ...job, cancelling: true };
    send(202, job);
  });
  fixture.handlers.set("POST /api/kernels/7.2.15/install", ({ send }) => {
    starts++;
    job = { ...job, status: "running", cancelling: false };
    send(202, job);
  });
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    await expect(
      page.getByRole("button", { name: "Cancel installation", exact: true }),
    ).toBeEnabled();
    await expect(
      page.getByRole("button", { name: "Cancel and delete", exact: true }),
    ).toBeEnabled();
    await page
      .getByRole("button", { name: "Cancel installation", exact: true })
      .click();
    await expect(
      page.getByRole("heading", {
        name: "Cancelling installation…",
        exact: true,
      }),
    ).toBeVisible();
    await expect(page.getByRole("progressbar")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Cancelling…", exact: true }),
    ).toBeDisabled();
    await page.reload();
    await expect(
      page.getByRole("heading", {
        name: "Cancelling installation…",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Cancel and delete", exact: true }),
    ).toBeDisabled();
    expect(cancellations).toBe(1);
    job = {
      ...job,
      status: "cancelled",
      cancelling: false,
      endedAtMs: Date.now(),
    };
    await expect(
      page.getByRole("button", { name: "Retry installation", exact: true }),
    ).toBeEnabled({ timeout: 8000 });
    await expect(page.locator(".kernel-table")).toContainText(
      "Installation cancelled",
    );
    await expect(page.getByRole("progressbar")).toHaveCount(0);
    await page
      .getByRole("button", { name: "Retry installation", exact: true })
      .click();
    await expect(page.getByRole("progressbar")).toHaveCount(1);
    expect(starts).toBe(1);
    expect(cancellations).toBe(1);
  } finally {
    await browser.close();
    await fixture.close();
  }
});

test("deleting a running kernel waits for cleanup and leaves other queued kernels untouched", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1000 },
  });
  fixture.state.kernels = ["7.2.15", "7.3"].map((version) => ({
    version,
    architecture: "aarch64",
    installed: false,
    inUse: false,
  }));
  const jobs = {
    "7.2.15": {
      version: "7.2.15",
      status: "running",
      cancelling: false,
      log: "building Linux 7.2.15",
    },
    7.3: {
      version: "7.3",
      status: "running",
      cancelling: false,
      log: "waiting for the host kernel builder",
    },
  };
  for (const version of Object.keys(jobs))
    fixture.handlers.set(`GET /api/kernels/${version}/install`, ({ send }) =>
      send(200, jobs[version]),
    );
  let deleted = [];
  let releaseDeletion;
  const deletion = new Promise((resolve) => {
    releaseDeletion = resolve;
  });
  fixture.handlers.set("DELETE /api/kernels/7.2.15", async ({ send }) => {
    deleted.push("7.2.15");
    await deletion;
    jobs["7.2.15"] = {
      ...jobs["7.2.15"],
      status: "idle",
      cancelling: false,
      log: "",
    };
    send(204);
  });
  page.on("dialog", (dialog) => dialog.accept());
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    await page
      .getByRole("button", { name: "Cancel and delete", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Deleting…", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("heading", {
        name: "Cancelling installation…",
        exact: true,
      }),
    ).toBeVisible();
    await expect(page.getByRole("progressbar")).toHaveCount(0);
    expect(deleted).toEqual(["7.2.15"]);
    releaseDeletion();
    await expect(
      page.getByRole("button", { name: "Build kernel", exact: true }),
    ).toBeEnabled();
    await expect(
      page.getByRole("button", { name: "Delete", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.locator(".kernel-table tr").filter({
        has: page.getByRole("button", { name: "7.3", exact: true }),
      }),
    ).toContainText("Waiting for build");
    expect(jobs["7.3"].status).toBe("running");
  } finally {
    releaseDeletion();
    await browser.close();
    await fixture.close();
  }
});

test("registry kernels show download and extraction steps without a local build", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const kernel = {
    version: "7.2.15",
    architecture: "aarch64",
    image: "Image-7.2.15-aarch64",
    imageSha256: "",
    packageSha256: "a".repeat(64),
    installed: false,
    inUse: false,
    installSource: "registry",
    publishedAt: "2026-11-02T00:00:00Z",
    endOfLife: true,
    packageUrl:
      "https://registry.firecrab.dev/kernel/7.2.15/r2/aarch64/vmlinux-7.2.15.tar.zst",
  };
  let log = "kernel package installation started for 7.2.15";
  let status = "idle";
  fixture.handlers.set("GET /api/kernels", ({ send }) => send(200, [kernel]));
  fixture.handlers.set("GET /api/kernels/7.2.15/install", ({ send }) =>
    send(200, {
      version: kernel.version,
      status,
      log: status === "idle" ? "" : log,
      cancelling: false,
      downloadedBytes: 512,
      totalBytes: 1024,
    }),
  );
  fixture.handlers.set("POST /api/kernels/7.2.15/install", ({ send }) => {
    status = "running";
    send(202, { version: kernel.version, status, log, cancelling: false });
  });
  try {
    await page.goto(`${fixture.url}/#/kernels`);
    await page
      .getByRole("button", { name: kernel.version, exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Published", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Build kernel", exact: true }),
    ).toHaveCount(0);
    await page
      .getByRole("button", { name: "Download kernel", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Resolve package", exact: true }),
    ).toBeVisible();
    log += "\ndownloading kernel package " + kernel.packageUrl;
    await expect(
      page.getByRole("heading", { name: "Download", exact: true }),
    ).toBeVisible();
    await expect(page.getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "50",
    );
    log += "\nextracting kernel package";
    await expect(
      page.getByRole("heading", { name: "Extract", exact: true }),
    ).toBeVisible();
    await expect(page.locator("body")).not.toContainText(
      "Waiting for the shared host builder",
    );
    log += "\nverifying kernel package";
    await expect(
      page.getByRole("heading", { name: "Verify", exact: true }),
    ).toBeVisible();
    await expect(page.locator("body")).not.toContainText("Build percentage");
    await page.screenshot({
      path: "/private/tmp/firecrab-registry-kernel-progress.png",
      fullPage: true,
    });
    log += "\ninstalling kernel package";
    await expect(
      page.getByRole("heading", { name: "Install", exact: true }),
    ).toBeVisible();
    kernel.installed = true;
    status = "succeeded";
    await expect(page.getByRole("progressbar")).toHaveCount(0);
  } finally {
    await browser.close();
    await fixture.close();
  }
});

test("kernel download attribution follows the API instead of claiming a fixed registry", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.handlers.set("GET /api/kernels/7.2.15/install", ({ send }) =>
    send(200, { version: "7.2.15", status: "idle", log: "" }),
  );
  try {
    for (const width of [390, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      for (const [installSource, packageUrl] of [
        [
          "registry",
          "https://registry.firecrab.dev/kernel/7.2.15/r2/aarch64/vmlinux-7.2.15.tar.zst",
        ],
        [
          undefined,
          "https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.2.15.tar.xz",
        ],
        [
          "registry",
          "https://kernels.example.test/kernel/7.2.15/aarch64/package.tar.zst",
        ],
      ]) {
        fixture.state.kernels = [
          {
            version: "7.2.15",
            architecture: "aarch64",
            installed: false,
            inUse: false,
            installSource,
            packageUrl,
          },
        ];
        await page.goto(`${fixture.url}/#/kernels`);
        const footer = page.locator(".kernel-source-footer");
        const source = new URL(packageUrl);
        await expect(
          footer.getByRole("link", { name: source.host, exact: true }),
        ).toHaveAttribute("href", source.origin);
        if (source.host !== "registry.firecrab.dev")
          await expect(footer).not.toContainText("registry.firecrab.dev");
        expect(
          await footer.evaluate(
            (element) => element.scrollWidth <= element.clientWidth,
          ),
        ).toBe(true);
        await page.getByRole("button", { name: "7.2.15", exact: true }).click();
        await expect(
          page
            .getByRole("complementary")
            .getByRole("link", { name: packageUrl, exact: true }),
        ).toHaveAttribute("href", packageUrl);
        await page
          .getByRole("complementary")
          .getByRole("button", { name: "Close detail", exact: true })
          .click();
      }
    }
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
