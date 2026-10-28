import { expect, test, type Page } from "@playwright/test";
import { apiUrl } from "../src/constants.js";

// B18/B19 run against the real API. The fake ECR host never resolves, so
// nothing is pulled and an existing Docker Hub login is never touched.
const HOST = "000000000000.dkr.ecr.qa-ci-1.amazonaws.com";
const SECRET = `e2e-ecr-secret-${process.pid}`;

interface Connection {
  registry: string;
  username: string;
  provider: string;
}

async function connections(): Promise<{ rows: Connection[]; text: string }> {
  const response = await fetch(`${apiUrl()}/api/pipe/registries`);
  expect(response.status).toBe(200);
  const text = await response.text();
  return { rows: JSON.parse(text) as Connection[], text };
}

async function removeLogin(): Promise<void> {
  const response = await fetch(`${apiUrl()}/api/pipe/registries/${HOST}`, {
    method: "DELETE",
  });
  expect(
    response.ok,
    `cleanup DELETE ${HOST} returned ${response.status}`,
  ).toBe(true);
}

async function openCards(page: Page) {
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe");
  return page.locator("#content");
}

test.describe("Pipe registries against the API", () => {
  test.beforeAll(async () => {
    const { rows } = await connections();
    test.skip(
      rows.some((row) => row.registry === HOST),
      `${HOST} already has a login`,
    );
  });
  test.afterEach(removeLogin);

  test("B18 lists Account, AI and Registry cards and shows the API login list", async ({
    page,
  }) => {
    const { rows } = await connections();
    const pipe = await openCards(page);
    await expect(pipe.locator("a.pipe-row")).toHaveCount(9);
    await expect(pipe.locator("a.pipe-row", { hasText: /^Account/ })).toBeVisible();
    await expect(pipe.locator("a.pipe-row", { hasText: /^Pipe AI/ })).toBeVisible();
    const card = pipe.locator("a.pipe-row", { hasText: /^Registry/ });
    await card.click();
    await expect(page).toHaveURL(/#\/pipe\/registry$/);
    const panel = page.locator("#content");
    await expect(
      panel.getByRole("button", { name: "Connect registry" }),
    ).toBeEnabled();
    for (const row of rows)
      await expect(
        panel.getByRole("cell", { name: row.registry, exact: true }),
      ).toHaveCount(1);
    await expect(panel.getByRole("alert")).toHaveCount(0);
  });

  test("B19 saves an ECR login without exposing the secret and removes it", async ({
    page,
  }) => {
    const pipe = await openCards(page);
    await pipe.locator("a.pipe-row", { hasText: /^Registry/ }).click();
    const panel = page.locator("#content");
    await panel.getByRole("button", { name: "Connect registry" }).click();
    await page.getByLabel("Registry host", { exact: true }).fill(HOST);
    await page.getByLabel("Username", { exact: true }).fill("AWS");
    // ECR is inferred from the hostname; only Harbor needs a manual provider hint.
    await expect(page.getByRole("radiogroup", { name: "Provider", exact: true }).getByRole("radio", { name: "Default", exact: true })).toBeChecked();
    await page.getByLabel("Password / token", { exact: true }).fill(SECRET);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    const row = panel.getByRole("row").filter({ has: page.getByRole("cell", { name: HOST, exact: true }) });
    await expect(row).toHaveCount(1);

    const saved = await connections();
    expect(saved.rows.find((value) => value.registry === HOST)).toEqual({
      registry: HOST,
      username: "AWS",
      provider: "ecr",
    });
    expect(saved.text).not.toContain(SECRET);
    await page.reload();
    await expect(row).toHaveCount(1);
    expect(await page.content()).not.toContain(SECRET);

    await page.goto("/#/pipe");
    await pipe.getByRole("link", { name: /^Registry/ }).click();
    await row.getByRole("button", { name: "Details", exact: true }).click();
    page.once("dialog", (dialog) => void dialog.accept());
    await page
      .locator("#detail-panel").getByRole("button", { name: "Disconnect", exact: true })
      .click();
    await expect(row).toHaveCount(0);
    expect(
      (await connections()).rows.some((value) => value.registry === HOST),
    ).toBe(false);
  });
});
