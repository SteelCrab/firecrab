import { expect, test, type Page } from "@playwright/test";

type Phase = "idle" | "checking" | "downloading" | "verifying" | "applying" | "restarting" | "done" | "failed";

interface Record {
  phase: Phase;
  percent: number;
  target?: string;
  downloadedBytes?: number;
  totalBytes?: number;
  error?: string;
  pid?: number;
  updatedAtMs: number;
}

const IDLE: Record = { phase: "idle", percent: 0, updatedAtMs: 0 };

const NOTES = [
  "Summary line with **bold** and `code`.",
  "",
  "### Added",
  "",
  "- Each MicroVM's systemd unit runs under host-side ceilings, wrapped",
  "  onto a second line ([#123]).",
  "- A link [to the docs](https://example.com/docs) and one that must not work [click](javascript:window.__xss=1).",
  "- <img src=x onerror=\"window.__xss=1\"> stays text.",
  "",
  "### Fixed",
  "",
  "- Console replay ([#368], [#999]).",
  "",
  "```sh",
  "firecrab update --apply",
  "```",
  "",
  "[#123]: https://github.com/SteelCrab/firecrab/issues/123",
  "[#368]: https://github.com/SteelCrab/firecrab/pull/368",
].join("\n");

const CHECK = {
  current: "0.3.0",
  latest: "0.3.1",
  updateAvailable: true,
  notes: NOTES,
  releaseUrl: "https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1",
};

interface Harness {
  /** What `GET /api/update/progress` answers. */
  record: Record;
  /** The updater's pid that `POST /api/update` answers. */
  pid: number;
  posts: number;
  /** While true the API does not answer the progress poll, as during a restart. */
  down: boolean;
}

async function openDashboard(page: Page, locale: "en" | "ko", harness: Harness, check: object = CHECK) {
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname === "/api/update/progress") {
      if (harness.down) return route.abort("connectionrefused");
      return route.fulfill({ json: harness.record });
    }
    if (pathname === "/api/update" && request.method() === "POST") {
      harness.posts += 1;
      return route.fulfill({ status: 202, json: { current: "0.3.0", pid: harness.pid } });
    }
    if (pathname === "/api/update") return route.fulfill({ json: check });
    if (pathname === "/api/vms") return route.fulfill({ json: [] });
    if (["/api/images", "/api/micro-networks", "/api/storage", "/api/shells"].includes(pathname)) {
      return route.fulfill({ json: [] });
    }
    return route.fulfill({ status: 503, json: { error: { message: "unused mock endpoint" } } });
  });
  await page.addInitScript((language) => localStorage.setItem("firecrab.locale", language), locale);
  await page.goto("/#/vms");
}

function harness(overrides: Partial<Harness> = {}): Harness {
  return { record: IDLE, pid: 4242, posts: 0, down: false, ...overrides };
}

const dialog = (page: Page) => page.getByRole("dialog");
const gauge = (page: Page) => page.getByRole("progressbar");
const stages = (page: Page) => page.locator(".update-stage");

test.describe("Update dialog @dashboard", () => {
  test("shows the release notes before anything is started, without trusting them", async ({ page }, testInfo) => {
    const state = harness();
    await openDashboard(page, "en", state);
    await page.getByRole("button", { name: "Update", exact: true }).click();

    await expect(dialog(page)).toBeVisible();
    await expect(dialog(page).getByRole("heading", { name: "Update to v0.3.1" })).toBeVisible();
    await expect(dialog(page)).toContainText("Installed v0.3.0");
    await expect(dialog(page).getByRole("heading", { name: "What's new" })).toBeVisible();

    const notes = page.locator(".release-notes");
    await expect(notes.getByRole("heading", { name: "Added" })).toBeVisible();
    await expect(notes.getByRole("heading", { name: "Fixed" })).toBeVisible();
    await expect(notes.locator("strong")).toHaveText("bold");
    await expect(notes.locator("p code")).toHaveText("code");
    await expect(notes.locator("pre code")).toHaveText("firecrab update --apply");
    // A wrapped list item is one item, and its reference link resolves.
    await expect(notes.locator("li").first()).toContainText("host-side ceilings, wrapped onto a second line");
    const issue = notes.locator('a[href="https://github.com/SteelCrab/firecrab/issues/123"]');
    await expect(issue).toHaveText("#123");
    await expect(issue).toHaveAttribute("target", "_blank");
    await expect(issue).toHaveAttribute("rel", /noopener/);
    await expect(notes.locator('a[href="https://example.com/docs"]')).toHaveText("to the docs");
    // A reference nobody defined stays as written.
    await expect(notes).toContainText("[#999]");

    // Untrusted text is text: no script link, no element from raw HTML.
    await expect(notes.locator('a[href^="javascript:"]')).toHaveCount(0);
    await expect(notes).toContainText("click");
    await expect(notes.locator("img")).toHaveCount(0);
    await expect(notes).toContainText('<img src=x onerror="window.__xss=1"> stays text.');
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss)).toBeUndefined();

    await expect(dialog(page).getByRole("link", { name: "View the release on GitHub" })).toHaveAttribute(
      "href",
      CHECK.releaseUrl,
    );
    await page.screenshot({ path: testInfo.outputPath("review.png"), fullPage: true });

    await page.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog(page)).toHaveCount(0);
    expect(state.posts).toBe(0);
  });

  test("shows a gauge, a percent, and the stage while it updates, through the restart", async ({ page }, testInfo) => {
    const state = harness();
    await openDashboard(page, "en", state);
    await page.getByRole("button", { name: "Update", exact: true }).click();
    await page.getByRole("button", { name: "Update now" }).click();
    expect(state.posts).toBe(1);

    await expect(dialog(page).getByRole("heading", { name: "Updating to v0.3.1…" })).toBeVisible();
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "0");
    await expect(dialog(page)).toContainText("Starting the updater…");
    await expect(page.getByRole("button", { name: "Cancel" })).toHaveCount(0);
    await expect(dialog(page)).toContainText("Keep this page open");

    state.record = {
      phase: "downloading",
      percent: 47,
      target: "0.3.1",
      downloadedBytes: 5_242_880,
      totalBytes: 11_178_942,
      pid: 4242,
      updatedAtMs: Date.now(),
    };
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "47");
    await expect(page.locator(".gauge-percent")).toHaveText("47%");
    await expect(dialog(page)).toContainText("Downloading — 5.0 MiB of 10.7 MiB");
    await expect(stages(page)).toHaveClass([/is-done/, /is-active/, /is-pending/, /is-pending/, /is-pending/]);
    await page.screenshot({ path: testInfo.outputPath("running.png"), fullPage: true });

    state.record = { ...state.record, phase: "applying", percent: 85, downloadedBytes: undefined, totalBytes: undefined };
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "85");
    await expect(stages(page)).toHaveClass([/is-done/, /is-done/, /is-done/, /is-active/, /is-pending/]);

    // The API goes down to restart: that is the restart, not a failure.
    state.down = true;
    await expect(dialog(page)).toContainText("Restarting the services");
    await expect(stages(page)).toHaveClass([/is-done/, /is-done/, /is-done/, /is-done/, /is-active/]);
    await expect(dialog(page).getByRole("heading", { name: "Update failed" })).toHaveCount(0);

    // The API that comes back runs the new version, and says the run is done.
    state.down = false;
    state.record = { ...state.record, phase: "done", percent: 100 };
    await expect(dialog(page).getByRole("heading", { name: "Updated to v0.3.1" })).toBeVisible();
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "100");
    await expect(stages(page)).toHaveClass(Array(5).fill(/is-done/));
    await expect(page.getByRole("button", { name: "Reload" })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("done.png"), fullPage: true });
  });

  test("reports a failure with where it stopped, and tries again", async ({ page }, testInfo) => {
    const state = harness();
    await openDashboard(page, "en", state);
    await page.getByRole("button", { name: "Update", exact: true }).click();
    await page.getByRole("button", { name: "Update now" }).click();

    state.record = {
      phase: "failed",
      percent: 85,
      target: "0.3.1",
      error: "network helper rejected the update: layout is not this host's install",
      pid: 4242,
      updatedAtMs: Date.now(),
    };
    await expect(dialog(page).getByRole("heading", { name: "Update failed" })).toBeVisible();
    await expect(dialog(page)).toContainText("network helper rejected the update: layout is not this host's install");
    await expect(stages(page)).toHaveClass([/is-done/, /is-done/, /is-done/, /is-failed/, /is-pending/]);
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "85");
    await page.screenshot({ path: testInfo.outputPath("failed.png"), fullPage: true });

    // Trying again follows the new updater, not the failed run's record.
    state.pid = 5151;
    await page.getByRole("button", { name: "Try again" }).click();
    expect(state.posts).toBe(2);
    await expect(dialog(page).getByRole("heading", { name: "Updating to v0.3.1…" })).toBeVisible();
    await expect(dialog(page)).toContainText("Starting the updater…");
    await page.waitForTimeout(2500);
    await expect(dialog(page).getByRole("heading", { name: "Update failed" })).toHaveCount(0);

    state.record = { phase: "verifying", percent: 80, target: "0.3.1", pid: 5151, updatedAtMs: Date.now() };
    await expect(stages(page)).toHaveClass([/is-done/, /is-done/, /is-active/, /is-pending/, /is-pending/]);
  });

  test("ignores the record of an earlier run until its own updater reports", async ({ page }) => {
    // An update to 0.3.0 finished earlier; its record is still there.
    const state = harness({ record: { phase: "done", percent: 100, target: "0.3.0", pid: 111, updatedAtMs: Date.now() - 86_400_000 } });
    await openDashboard(page, "en", state);
    await page.getByRole("button", { name: "Update", exact: true }).click();
    state.pid = 222;
    await page.getByRole("button", { name: "Update now" }).click();

    await page.waitForTimeout(2500);
    await expect(dialog(page).getByRole("heading", { name: "Updating to v0.3.1…" })).toBeVisible();
    await expect(dialog(page).getByRole("heading", { name: /^Updated/ })).toHaveCount(0);

    state.record = { phase: "checking", percent: 0, target: "0.3.1", pid: 222, updatedAtMs: Date.now() };
    await expect(dialog(page)).toContainText("Looking up the newest release…");
  });

  test("a page opened mid-update follows it instead of offering it again", async ({ page }) => {
    const state = harness({
      record: { phase: "downloading", percent: 30, target: "0.3.1", downloadedBytes: 3_000_000, totalBytes: 10_000_000, pid: 77, updatedAtMs: Date.now() },
    });
    await openDashboard(page, "en", state);

    await expect(dialog(page).getByRole("heading", { name: "Updating to v0.3.1…" })).toBeVisible();
    await expect(gauge(page)).toHaveAttribute("aria-valuenow", "30");
    await expect(dialog(page).getByRole("heading", { name: "What's new" })).toHaveCount(0);
    expect(state.posts).toBe(0);

    state.record = { ...state.record, phase: "done", percent: 100 };
    await expect(dialog(page).getByRole("heading", { name: "Updated to v0.3.1" })).toBeVisible();
  });

  test("a release without notes still offers the update, in Korean", async ({ page }, testInfo) => {
    const state = harness();
    await openDashboard(page, "ko", state, { current: "0.3.0", latest: "0.3.1", updateAvailable: true });
    await page.getByRole("button", { name: "업데이트", exact: true }).click();

    await expect(dialog(page).getByRole("heading", { name: "v0.3.1로 업데이트" })).toBeVisible();
    await expect(dialog(page)).toContainText("이 버전에는 릴리스 노트가 없습니다.");
    await expect(dialog(page).getByRole("link")).toHaveCount(0);
    await page.screenshot({ path: testInfo.outputPath("review-ko.png"), fullPage: true });

    await page.getByRole("button", { name: "지금 업데이트" }).click();
    state.record = { phase: "downloading", percent: 20, target: "0.3.1", downloadedBytes: 2_000_000, totalBytes: 10_000_000, pid: 4242, updatedAtMs: Date.now() };
    await expect(dialog(page)).toContainText("내려받는 중 — 1.9 MiB / 9.5 MiB");
    await expect(dialog(page)).toContainText("번들 내려받기");
  });

  test("Escape closes the dialog before the update starts but not while it runs", async ({ page }) => {
    const state = harness();
    await openDashboard(page, "en", state);
    await page.getByRole("button", { name: "Update", exact: true }).click();
    await expect(dialog(page)).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(dialog(page)).toHaveCount(0);

    await page.getByRole("button", { name: "Update", exact: true }).click();
    await page.getByRole("button", { name: "Update now" }).click();
    await page.keyboard.press("Escape");
    await expect(dialog(page)).toBeVisible();
  });
});
