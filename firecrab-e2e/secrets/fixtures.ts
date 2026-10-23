import { spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { closeSync, existsSync, openSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test as base, expect, type Page, type TestInfo } from "@playwright/test";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const binary = path.resolve(process.env.FIRECRAB_SECRETS_E2E_API_BIN
  ?? path.join(repo, "target/debug/firecrab-api"));
const frontend = path.join(repo, "firecrab-frontend/dist");
const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export interface SecretRow {
  id: string; name: string; description: string; version: number;
  source: { connectionId: string; connectionName: string } | null;
}

export class SecretServer {
  private child?: ChildProcess;
  private stopped: Promise<unknown> = Promise.resolve();
  readonly vault: string;
  url = "";

  constructor(readonly root: string, private log: string) {
    this.vault = path.join(root, "data/firecrab.secrets");
  }

  async start() {
    if (!this.url) {
      const reservation = createServer();
      reservation.listen(0, "127.0.0.1");
      await once(reservation, "listening");
      const address = reservation.address();
      if (!address || typeof address === "string") throw new Error("No loopback port allocated");
      this.url = `http://127.0.0.1:${address.port}`;
      await new Promise<void>((resolve, reject) => reservation.close((error) => error ? reject(error) : resolve()));
    }
    // Use an empty environment so installed host paths, credentials, and service
    // settings cannot reach a disposable API. Loopback providers remain reachable.
    const log = openSync(this.log, "a", 0o600);
    try {
      this.child = spawn(binary, [], {
        cwd: this.root,
        env: {
          PATH: process.env.PATH,
          HOME: this.root,
          XDG_CACHE_HOME: path.join(this.root, "cache"),
          FIRECRAB_BIND_ADDR: new URL(this.url).host,
          FIRECRAB_ALLOWED_ORIGINS: this.url,
          FIRECRAB_IMAGE_ROOT: path.join(this.root, "images"),
          FIRECRAB_NET_HELPER_SOCK: path.join(this.root, "unused-helper.sock"),
          FIRECRAB_STATIC_ROOT: frontend,
          HTTP_PROXY: "http://127.0.0.1:9",
          HTTPS_PROXY: "http://127.0.0.1:9",
          NO_PROXY: "127.0.0.1,localhost,::1",
          RUST_LOG: "firecrab_api=warn",
        },
        stdio: ["ignore", log, log],
      });
    } finally { closeSync(log); }
    let exited = false;
    this.stopped = new Promise<void>((resolve) => {
      this.child!.once("error", () => { exited = true; resolve(); });
      this.child!.once("exit", () => { exited = true; resolve(); });
    });
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline && !exited) {
      try {
        const response = await fetch(`${this.url}/api/host`, { signal: AbortSignal.timeout(500) });
        if (response.ok) return;
      } catch { /* Listener is not ready yet. */ }
      await delay(50);
    }
    throw new Error(`Disposable API did not start; see ${this.log}`);
  }

  async stop() {
    if (!this.child) return;
    this.child.kill("SIGTERM");
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        this.stopped,
        new Promise<void>((resolve) => { timer = setTimeout(() => { this.child?.kill("SIGKILL"); resolve(); }, 3_000); }),
      ]);
      await this.stopped;
    } finally { clearTimeout(timer); this.child = undefined; }
  }

  async restart() { await this.stop(); await this.start(); }

  async request<T = Record<string, unknown>>(method: string, endpoint: string, data?: unknown) {
    const response = await fetch(`${this.url}/api/secrets${endpoint}`, {
      method,
      headers: data === undefined ? {} : { "Content-Type": "application/json" },
      body: data === undefined ? undefined : JSON.stringify(data),
      signal: AbortSignal.timeout(15_000),
    });
    expect(response.headers.get("cache-control")).toBe("no-store");
    return { status: response.status, body: await response.json() as T };
  }

  async rows() {
    const result = await this.request<SecretRow[]>("GET", "");
    expect(result.status).toBe(200);
    return result.body;
  }

  async create(name: string, value: string) {
    const result = await this.request<SecretRow>("POST", "", { name, value });
    expect(result.status).toBe(201);
    return result.body;
  }

  async reveal(id: string) {
    const result = await this.request<{ value: string }>("POST", `/${id}/reveal`);
    expect(result.status).toBe(200);
    return result.body.value;
  }
}

async function server(label: string, info: TestInfo) {
  const root = await mkdtemp(path.join(os.tmpdir(), `qa-pipesecrets-${label}-`));
  return new SecretServer(root, info.outputPath(`${label}-api.log`));
}

export const test = base.extend<{ local: SecretServer; remote: SecretServer }>({
  local: async ({}, use, info) => {
    if (process.platform !== "linux") throw new Error("Real Secrets E2E requires a Linux API host; use Linux or managed Debian");
    if (!existsSync(binary) || !existsSync(path.join(frontend, "index.html"))) {
      throw new Error("Build firecrab-api and firecrab-frontend first; see the Secrets E2E README");
    }
    const local = await server("local", info);
    try { await local.start(); await use(local); }
    finally { await local.stop(); await rm(local.root, { recursive: true, force: true }); expect(existsSync(local.root)).toBe(false); }
  },
  remote: async ({}, use, info) => {
    const remote = await server("remote", info);
    try { await remote.start(); await use(remote); }
    finally { await remote.stop(); await rm(remote.root, { recursive: true, force: true }); expect(existsSync(remote.root)).toBe(false); }
  },
});

export async function openSecrets(page: Page, local: SecretServer) {
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto(`${local.url}/#/secrets`);
  await expect(page.getByRole("button", { name: "New secret", exact: true })).toBeEnabled();
}

export async function connect(page: Page, remoteUrl: string) {
  await page.getByRole("tab", { name: /External connections/ }).click();
  await page.getByRole("button", { name: "Add connection", exact: true }).click();
  await page.getByLabel("Connection name", { exact: true }).fill("qa-remote");
  await page.getByLabel("Provider", { exact: true }).selectOption("firecrab");
  await page.getByLabel("Base URL", { exact: true }).fill(remoteUrl);
  await page.getByRole("button", { name: "Save connection", exact: true }).click();
  await expect(page.getByRole("heading", { name: "qa-remote", exact: true })).toBeVisible();
}

export { expect };
