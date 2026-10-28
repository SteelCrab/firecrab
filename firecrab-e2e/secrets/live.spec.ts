import { once } from "node:events";
import { readFile, stat, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";
import { connect, expect, openSecrets, test } from "./fixtures.js";

test("SEC9: CRUD survives an API restart and leaves only encrypted values on disk", async ({ page, local }) => {
  const value = "qa-only-multiline-password\n두 번째 줄";
  await openSecrets(page, local);
  await page.getByRole("button", { name: "New secret", exact: true }).click();
  await page.getByLabel("Secret name", { exact: true }).fill("qa-DB_PASSWORD");
  await page.getByLabel("Secret value", { exact: true }).fill(value);
  await page.getByRole("button", { name: "Save secret", exact: true }).click();
  await expect(page.getByLabel("qa-DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
  const [created] = await local.rows();
  expect(created).not.toHaveProperty("value");
  await page.getByRole("button", { name: "Edit qa-DB_PASSWORD", exact: true }).click();
  await expect(page.getByLabel("Secret value", { exact: true })).toBeHidden();
  await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue("");
  await page.getByLabel("Description", { exact: true }).fill("qa-description-only");
  await page.getByRole("button", { name: "Save secret", exact: true }).click();
  await expect(page.getByText("qa-description-only", { exact: true })).toBeVisible();
  expect(await local.reveal(created.id)).toBe(value);
  for (const name of ["vault.bin", "master.key"]) {
    const file = path.join(local.vault, name);
    expect((await stat(file)).mode & 0o777).toBe(0o600);
    expect((await readFile(file)).includes(Buffer.from(value))).toBe(false);
  }
  expect((await stat(local.vault)).mode & 0o777).toBe(0o700);
  await local.restart();
  await page.reload();
  await page.getByRole("button", { name: "Reveal qa-DB_PASSWORD", exact: true }).click();
  await expect(page.getByLabel("qa-DB_PASSWORD value", { exact: true })).toHaveText(value);
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Delete qa-DB_PASSWORD", exact: true }).click();
  await expect(page.getByRole("heading", { name: "No secrets yet", exact: true })).toBeVisible();
  await local.restart();
  expect(await local.rows()).toEqual([]);
});

test("SEC10: import applies a captured snapshot and keeps local copies after disconnect", async ({ page, local, remote }) => {
  const existing = await local.create("qa-EXISTING", "qa-local-old");
  const upstream = await remote.create("qa-EXISTING", "qa-remote-snapshot");
  await remote.create("qa-NEW", "qa-new-snapshot");
  await openSecrets(page, local);
  await connect(page, remote.url);
  const previewResponse = page.waitForResponse((response) => response.url().endsWith("/preview"));
  await page.getByRole("button", { name: "Import from qa-remote", exact: true }).click();
  const preview = await (await previewResponse).json();
  expect(preview.changes).toEqual([{ name: "qa-EXISTING", change: "update" }, { name: "qa-NEW", change: "add" }]);
  expect(JSON.stringify(preview)).not.toContain("qa-remote-snapshot");
  await expect(page.getByLabel("Apply qa-EXISTING", { exact: true })).not.toBeChecked();
  await expect(page.getByLabel("Apply qa-NEW", { exact: true })).toBeChecked();
  expect((await local.rows()).map((row) => row.name)).toEqual(["qa-EXISTING"]);
  expect((await remote.request("PUT", `/${upstream.id}`, { name: upstream.name, value: "qa-remote-later", version: upstream.version })).status).toBe(200);
  await page.getByLabel("Apply qa-EXISTING", { exact: true }).check();
  await page.getByRole("button", { name: "Import selected (2)", exact: true }).click();
  await expect(page.getByText("2 secret(s) applied.", { exact: true })).toBeVisible();
  const imported = await local.rows();
  expect(imported[0].id).toBe(existing.id);
  expect(imported[0].source?.connectionName).toBe("qa-remote");
  expect(await local.reveal(existing.id)).toBe("qa-remote-snapshot");
  expect((await local.request("POST", `/plans/${preview.id}/apply`, { names: ["qa-NEW"] })).status).toBe(404);
  await remote.request("DELETE", `/${upstream.id}`);
  await remote.stop();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Delete connection qa-remote", exact: true }).click();
  await expect(page.getByRole("button", { name: "Import from qa-remote", exact: true })).toHaveCount(0);
  await local.restart();
  expect(await local.rows()).toEqual(imported);
  expect(await local.reveal(imported[1].id)).toBe("qa-new-snapshot");
});

test("SEC11: upload rejects a changed remote and applies only the selected local values once", async ({ page, local, remote }) => {
  await local.create("qa-UPLOAD", "qa-upload-value");
  await local.create("qa-UNSELECTED", "qa-should-stay-local");
  const target = await remote.create("qa-UPLOAD", "qa-before-preview");
  await openSecrets(page, local);
  await connect(page, remote.url);
  await page.getByRole("tab", { name: /Internal/ }).click();
  await page.getByLabel("Select qa-UPLOAD", { exact: true }).check();
  await page.getByLabel("Upload destination", { exact: true }).selectOption({ label: "qa-remote · Firecrab · PipeSecrets" });
  await page.getByRole("button", { name: "Preview upload", exact: true }).click();
  await expect(page.getByLabel("Apply qa-UPLOAD", { exact: true })).not.toBeChecked();
  expect((await remote.request("PUT", `/${target.id}`, { name: target.name, value: "qa-concurrent-remote", version: 1 })).status).toBe(200);
  await page.getByLabel("Apply qa-UPLOAD", { exact: true }).check();
  await page.getByRole("button", { name: "Upload selected (1)", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("The values changed");
  expect(await remote.reveal(target.id)).toBe("qa-concurrent-remote");
  const response = page.waitForResponse((response) => response.url().endsWith("/preview"));
  await page.getByRole("button", { name: "Preview upload", exact: true }).click();
  const preview = await (await response).json();
  await page.getByLabel("Apply qa-UPLOAD", { exact: true }).check();
  await page.getByRole("button", { name: "Upload selected (1)", exact: true }).click();
  await expect(page.getByText("1 secret(s) applied.", { exact: true })).toBeVisible();
  await remote.restart();
  expect((await remote.rows()).map((row) => row.name)).toEqual(["qa-UPLOAD"]);
  expect(await remote.reveal(target.id)).toBe("qa-upload-value");
  expect((await local.request("POST", `/plans/${preview.id}/apply`, { names: ["qa-UPLOAD"] })).status).toBe(404);
});

test("SEC12: stale editor versions and stale transfer previews cannot overwrite local data", async ({ page, local, remote }) => {
  const row = await local.create("qa-CONFLICT", "qa-original");
  await remote.create("qa-CONFLICT", "qa-import");
  await openSecrets(page, local);
  await page.getByRole("button", { name: "Edit qa-CONFLICT", exact: true }).click();
  await page.getByLabel("Replace secret value", { exact: true }).check();
  await page.getByLabel("Secret value", { exact: true }).fill("qa-stale-editor");
  expect((await local.request("PUT", `/${row.id}`, { name: row.name, value: "qa-newer-local", version: 1 })).status).toBe(200);
  await page.getByRole("button", { name: "Save secret", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("The values changed");
  await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue("qa-stale-editor");
  expect(await local.reveal(row.id)).toBe("qa-newer-local");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await connect(page, remote.url);
  await page.getByRole("button", { name: "Import from qa-remote", exact: true }).click();
  await expect(page.getByLabel("Apply qa-CONFLICT", { exact: true })).toBeVisible();
  expect((await local.request("PUT", `/${row.id}`, { name: row.name, value: "qa-after-preview", version: 2 })).status).toBe(200);
  await page.getByLabel("Apply qa-CONFLICT", { exact: true }).check();
  await page.getByRole("button", { name: "Import selected (1)", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("The values changed");
  expect(await local.reveal(row.id)).toBe("qa-after-preview");
});

test("SEC13: Firecrab proxy credentials survive edits and restarts; provider failures stay masked", async ({ page, local }) => {
  let deny = true;
  const calls: { auth?: string; path: string }[] = [];
  const remoteId = "11111111-1111-4111-8111-111111111113";
  const provider = createServer((request, response) => {
    const url = new URL(request.url!, "http://localhost");
    calls.push({ auth: request.headers.authorization, path: url.pathname });
    response.writeHead(deny ? 403 : 200, { "Content-Type": "application/json" });
    response.end(JSON.stringify(deny
      ? { error: "qa-raw-error-private-token" }
      : url.pathname.endsWith("/reveal")
        ? { value: "qa-proxy-copy", version: 1 }
        : [{ id: remoteId, name: "qa-PROXY", description: "", version: 1 }]));
  });
  provider.listen(0, "127.0.0.1");
  await once(provider, "listening");
  const address = provider.address();
  if (!address || typeof address === "string") throw new Error("Provider fixture did not bind");
  try {
    await openSecrets(page, local);
    await page.getByRole("tab", { name: /External connections/ }).click();
    await page.getByRole("button", { name: "Add connection", exact: true }).click();
    await page.getByRole("radiogroup", { name: "Provider", exact: true }).getByRole("radio", { name: "Firecrab · PipeSecrets", exact: true }).check();
    await page.getByLabel("Connection name", { exact: true }).fill("qa-proxy");
    await page.getByLabel("Base URL", { exact: true }).fill(`http://127.0.0.1:${address.port}`);
    await page.getByLabel("Bearer token", { exact: true }).fill("qa-private-credential");
    await page.getByRole("button", { name: "Save connection", exact: true }).click();
    await page.getByRole("button", { name: "Edit connection qa-proxy", exact: true }).click();
    await expect(page.getByLabel("Bearer token", { exact: true })).toHaveValue("");
    await page.getByRole("button", { name: "Save connection", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Edit connection", exact: true })).toHaveCount(0);
    expect(JSON.stringify((await local.request("GET", "/connections")).body)).not.toContain("qa-private-credential");
    expect((await readFile(path.join(local.vault, "vault.bin"))).includes(Buffer.from("qa-private-credential"))).toBe(false);
    await local.restart();
    await page.reload();
    await page.getByRole("tab", { name: /External connections/ }).click();
    const failed = page.waitForResponse((response) => response.url().endsWith("/preview"));
    await page.getByRole("button", { name: "Import from qa-proxy", exact: true }).click();
    const failure = await failed;
    expect(failure.status()).toBe(503);
    expect(await failure.text()).not.toContain("qa-raw-error-private-token");
    await expect(page.getByRole("alert")).toContainText("External service request failed");
    deny = false;
    await page.getByRole("button", { name: "Import from qa-proxy", exact: true }).click();
    await page.getByRole("button", { name: "Import selected (1)", exact: true }).click();
    await expect(page.getByText("1 secret(s) applied.", { exact: true })).toBeVisible();
    const [imported] = await local.rows();
    expect(await local.reveal(imported.id)).toBe("qa-proxy-copy");
    expect(calls.length).toBeGreaterThanOrEqual(2);
    for (const call of calls) {
      expect(call.auth).toBe("Bearer qa-private-credential");
      expect(["/api/secrets", `/api/secrets/${remoteId}/reveal`]).toContain(call.path);
    }
  } finally {
    provider.closeAllConnections();
    await new Promise<void>((resolve, reject) => provider.close((error) => error ? reject(error) : resolve()));
  }
});

test("SEC14: a missing key or damaged vault fails closed without replacing stored data", async ({ page, local }) => {
  await local.create("qa-RECOVERY", "qa-recovery-value");
  const keyFile = path.join(local.vault, "master.key");
  const vaultFile = path.join(local.vault, "vault.bin");
  const key = await readFile(keyFile);
  const vault = await readFile(vaultFile);
  await local.stop();
  await unlink(keyFile);
  await local.start();
  await openSecretsForFailure();
  expect((await readFile(vaultFile)).equals(vault)).toBe(true);
  await expect(stat(keyFile)).rejects.toThrow();
  await local.stop();
  await writeFile(keyFile, key, { mode: 0o600 });
  const damaged = Buffer.from(vault);
  damaged[damaged.length - 1] ^= 1;
  await writeFile(vaultFile, damaged);
  await local.start();
  await openSecretsForFailure();
  expect((await readFile(vaultFile)).equals(damaged)).toBe(true);

  async function openSecretsForFailure() {
    await page.goto(`${local.url}/#/secrets`);
    await expect(page.getByRole("alert")).toContainText("PipeSecrets storage or its encryption key is unavailable");
    await expect(page.getByRole("button", { name: "New secret", exact: true })).toBeDisabled();
    expect((await local.request("GET", "")).status).toBe(503);
  }
});
