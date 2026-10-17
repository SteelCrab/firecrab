/**
 * Reload initial dashboard modules once when Chromium observes a host-network
 * change, such as a newly created guest bridge. Keep other failures visible.
 * @param {import("@playwright/test").Page} page
 * @param {string} hash
 */
export async function openDashboard(page, hash) {
  let networkChanged = false;
  const failed = (request) => {
    if (["document", "script"].includes(request.resourceType()) &&
        request.failure()?.errorText === "net::ERR_NETWORK_CHANGED") {
      networkChanged = true;
    }
  };
  page.on("requestfailed", failed);
  try {
    for (let attempt = 0; ; attempt += 1) {
      networkChanged = false;
      try {
        await page.goto(hash);
        await page.waitForFunction(
          () => (document.getElementById("root")?.childElementCount ?? 0) > 0,
          null,
          { timeout: 5000 },
        );
        return;
      } catch (error) {
        if (attempt === 1 || !networkChanged) throw error;
      }
    }
  } finally {
    page.off("requestfailed", failed);
  }
}
