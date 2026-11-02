import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

const operation = (id, state = "needs_recovery") => ({
  operationId: id,
  action: "delete_vm",
  state,
  retainDisks: false,
  executionGeneration: 0,
  ownership: {
    endpointId: "endpoint-one",
    resource: { kind: "vm", id: "7266e614-3d65-45d5-957f-14cd2b2a5b43" },
  },
});
async function fixture() {
  const server = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  server.handlers.set("GET /api/stacks", ({ send }) => send(200, []));
  server.handlers.set("GET /api/iac/owners", ({ send }) => send(200, []));
  return server;
}

test("Native recovery uses the original receipt and observes it after losing acknowledgement", async () => {
  const server = await fixture();
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 375, height: 812 } });
  let current = operation("original-operation");
  let posts = 0;
  let reads = 0;
  server.handlers.set("GET /api/iac/native-operations", ({ send }) =>
    send(200, {
      operations: current.state === "succeeded" ? [] : [current],
      nextBefore: null,
    }),
  );
  server.handlers.set(
    "POST /api/iac/native-operations/original-operation/recover",
    ({ req, send }) => {
      posts++;
      if (req.headers["x-firecrab-endpoint-id"] !== "endpoint-one")
        throw new Error("missing endpoint pin");
      current = { ...current, state: "succeeded", executionGeneration: 1 };
      send(503, { error: { message: "Acknowledgement lost" } });
    },
  );
  server.handlers.set(
    "GET /api/iac/native-operations/original-operation",
    ({ send }) => {
      reads++;
      send(200, current);
    },
  );
  try {
    await page.goto(`${server.url}/#/stacks`);
    const panel = page.getByRole("region", {
      name: "Native operations",
      exact: true,
    });
    await expect(
      panel.getByText("Delete private artifacts", { exact: true }),
    ).toBeVisible();
    await expect(panel.getByText("Delete VM", { exact: true })).toBeVisible();
    await expect(
      panel.getByText("Needs recovery", { exact: true }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      )
      .toBe(true);
    await panel
      .getByRole("button", { name: "Resume VM deletion", exact: true })
      .click();
    await expect(panel.getByRole("status")).toHaveText(
      "Original operation completed.",
    );
    expect(posts).toBe(1);
    expect(reads).toBe(1);
    await expect(
      panel.getByRole("button", { name: "Resume VM deletion", exact: true }),
    ).toHaveCount(0);
  } finally {
    await page.close();
    await browser.close();
    await server.close();
  }
});

test("Native work paginates, keeps running work disabled, and fails closed when lookup fails", async () => {
  const server = await fixture();
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  let unavailable = false;
  let olderRequests = 0;
  let posts = 0;
  server.handlers.set("GET /api/iac/native-operations", ({ url, send }) => {
    if (unavailable)
      return send(503, { error: { message: "Recorded work unavailable" } });
    if (url.searchParams.get("before") === "older") {
      olderRequests++;
      return send(200, {
        operations: [{ ...operation("older-operation"), retainDisks: true }],
        nextBefore: null,
      });
    }
    send(200, {
      operations: [operation("running-operation", "running")],
      nextBefore: "older",
    });
  });
  server.handlers.set(
    "POST /api/iac/native-operations/older-operation/recover",
    ({ send }) => {
      posts++;
      send(500, { error: { message: "Unexpected dispatch" } });
    },
  );
  try {
    await page.goto(`${server.url}/#/stacks`);
    const panel = page.getByRole("region", {
      name: "Native operations",
      exact: true,
    });
    await expect(
      panel.locator('[data-operation-id="running-operation"]'),
    ).toBeVisible();
    await expect(
      panel.getByRole("button", { name: "Resume VM deletion" }),
    ).toHaveCount(0);
    await panel
      .getByRole("button", { name: "Load more active operations" })
      .click();
    await expect(
      panel.getByText("Retain private artifacts", { exact: true }),
    ).toBeVisible();
    expect(olderRequests).toBeGreaterThan(0);
    unavailable = true;
    await expect(panel.getByRole("alert")).toContainText(
      "Recorded work unavailable",
      { timeout: 10000 },
    );
    await expect(
      panel.getByRole("button", { name: "Resume VM deletion", exact: true }),
    ).toBeDisabled();
    expect(posts).toBe(0);
  } finally {
    await page.close();
    await browser.close();
    await server.close();
  }
});

test("Native recovery reports an uncertain backend result without claiming it is running", async () => {
  const server = await fixture();
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  let current = {
    ...operation("failed-engine-operation"),
    action: "stop_queue_engine",
    retainDisks: undefined,
    errorCode: "engine_cleanup_incomplete",
  };
  let posts = 0;
  server.handlers.set("GET /api/iac/native-operations", ({ send }) =>
    send(200, { operations: [current], nextBefore: null }),
  );
  server.handlers.set(
    "POST /api/iac/native-operations/failed-engine-operation/recover",
    ({ send }) => {
      posts++;
      current = { ...current, executionGeneration: 1 };
      send(200, current);
    },
  );
  try {
    await page.goto(`${server.url}/#/stacks`);
    const panel = page.getByRole("region", {
      name: "Native operations",
      exact: true,
    });
    await panel
      .getByRole("button", { name: "Recover original operation" })
      .click();
    await expect(panel.getByRole("alert")).toContainText(
      "Original operation still needs recovery.",
    );
    await expect(panel.getByRole("status")).toHaveCount(0);
    await expect(
      panel.getByText("engine_cleanup_incomplete", { exact: true }),
    ).toBeVisible();
    await expect(
      panel.getByText("Needs recovery", { exact: true }),
    ).toBeVisible();
    expect(posts).toBe(1);
  } finally {
    await page.close();
    await browser.close();
    await server.close();
  }
});
