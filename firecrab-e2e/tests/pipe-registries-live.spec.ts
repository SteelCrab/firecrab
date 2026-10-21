import { expect, test, type Page } from "@playwright/test";
import { apiUrl } from "../src/constants.js";

// B18/B19 run against the real API. The fake ECR host never resolves, so
// nothing is pulled and an existing Docker Hub login is never touched.
const HOST = "000000000000.dkr.ecr.qa-ci-1.amazonaws.com";
const SECRET = `e2e-ecr-secret-${process.pid}`;

interface Connection { registry: string; username: string; provider: string }

async function connections(): Promise<{ rows: Connection[]; text: string }> {
  const response = await fetch(`${apiUrl()}/api/pipe/registries`);
  expect(response.status).toBe(200);
  const text = await response.text();
  return { rows: JSON.parse(text) as Connection[], text };
}

async function removeLogin(): Promise<void> {
  await fetch(`${apiUrl()}/api/pipe/registries/${HOST}`, { method: "DELETE" }).catch(() => undefined);
}

function cardStatus(count: number): string {
  return count ? `${count} connected` : "Not configured";
}

async function openCards(page: Page) {
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe");
  return page.getByRole("region", { name: "Pipe", exact: true });
}

test.describe("Pipe registries against the API", () => {
  test.beforeAll(async () => {
    const { rows } = await connections();
    test.skip(rows.some(row => row.registry === HOST), `${HOST} already has a login`);
  });
  test.afterEach(removeLogin);

  test("B18 lists the Registry card with the API login count and opens the registry panel", async ({ page }) => {
    const { rows } = await connections();
    const pipe = await openCards(page);
    await expect(pipe.getByRole("listitem")).toHaveCount(5);
    const card = pipe.getByRole("link", { name: "Pipe Registry", exact: true });
    await expect(card).toContainText(cardStatus(rows.length));
    await card.click();
    await expect(page).toHaveURL(/#\/pipe\/registry$/);
    const panel = page.getByRole("region", { name: "Pipe Registry", exact: true });
    await expect(panel.getByRole("button", { name: "Add registry" })).toBeEnabled();
    for (const row of rows) await expect(panel.getByRole("cell", { name: row.registry, exact: true })).toBeVisible();
    await expect(panel.getByRole("alert")).toHaveCount(0);
  });

  test("B19 saves an ECR login without exposing the secret and removes it", async ({ page }) => {
    const before = (await connections()).rows.length;
    const pipe = await openCards(page);
    await pipe.getByRole("link", { name: "Pipe Registry", exact: true }).click();
    const panel = page.getByRole("region", { name: "Pipe Registry", exact: true });
    await panel.getByRole("button", { name: "Add registry" }).click();
    await panel.getByLabel("Provider").selectOption("ecr");
    await panel.getByLabel("Registry hostname").fill(HOST);
    await panel.getByLabel("ECR login password").fill(SECRET);
    await panel.getByRole("button", { name: "Save login" }).click();
    const row = panel.getByRole("row").filter({ has: page.getByRole("cell", { name: HOST, exact: true }) });
    await expect(row).toBeVisible();

    const saved = await connections();
    expect(saved.rows.find(value => value.registry === HOST)).toEqual({ registry: HOST, username: "AWS", provider: "ecr" });
    expect(saved.text).not.toContain(SECRET);
    await page.reload();
    await expect(row).toBeVisible();
    expect(await page.content()).not.toContain(SECRET);

    await page.goto("/#/pipe");
    await expect(pipe.getByRole("link", { name: "Pipe Registry", exact: true })).toContainText(cardStatus(before + 1));
    await pipe.getByRole("link", { name: "Pipe Registry", exact: true }).click();
    await row.getByRole("button", { name: "Remove" }).click();
    await expect(row).toHaveCount(0);
    expect((await connections()).rows.some(value => value.registry === HOST)).toBe(false);
  });
});
