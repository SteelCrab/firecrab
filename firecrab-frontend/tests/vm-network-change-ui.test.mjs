import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

async function vmFixture(
  state = "stopped",
  { locale = "en", width = 1280, securityGroups = [] } = {},
) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  fixture.state.networks.push({
    ...fixture.state.networks[0],
    id: "network-2",
    name: "private",
    subnetCidr: "10.50.0.0/24",
    gateway: "10.50.0.1",
    internetEnabled: false,
  });
  fixture.state.securityGroups = securityGroups;
  fixture.state.vms = [
    vm(0, {
      state,
      microNetworkId: "network-1",
      securityGroupId: securityGroups[0]?.id ?? null,
    }),
  ];
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width, height: 900 } });
  await page.addInitScript(
    (value) => localStorage.setItem("firecrab.locale", value),
    locale,
  );
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const detail = page.getByRole("complementary", {
    name: locale === "ko" ? "MicroVM 상세" : "MicroVM detail",
  });
  await page.goto(`${fixture.url}/#/vms`);
  await page.getByRole("button", { name: "vm-00000", exact: true }).click();
  return {
    fixture,
    page,
    detail,
    errors,
    close: async () => {
      await browser.close();
      await fixture.close();
    },
  };
}

test("inactive VM network edit reuses catalog options and saves the selected network", async () => {
  const { fixture, page, detail, errors, close } = await vmFixture();
  try {
    fixture.handlers.set(
      `PUT /api/vms/${fixture.state.vms[0].id}`,
      ({ body, send }) => {
        Object.assign(fixture.state.vms[0], body);
        send(200, fixture.state.vms[0]);
      },
    );
    await detail
      .getByRole("button", { name: "Edit MicroNetwork", exact: true })
      .click();
    const picker = detail.getByLabel("MicroNetwork", { exact: true });
    await expect(picker.locator("option")).toContainText([
      "Select a MicroNetwork",
      "default (10.42.0.0/16)",
      "private (10.50.0.0/24)",
    ]);
    await picker.selectOption("network-2");
    await detail.getByRole("button", { name: "Save", exact: true }).click();
    await expect(detail).toContainText("private (10.50.0.0/24)");
    const request = fixture.requests.find(
      (request) => request.method === "PUT",
    );
    assert.equal(request.body.microNetworkId, "network-2");
    assert.equal(request.body.cpu, 2);
    assert.equal(request.body.ram, 1024);
    assert.equal(request.body.diskGb, 8);
    assert.equal(request.body.env, undefined);
    assert.deepEqual(errors, []);
  } finally {
    await close();
  }
});

test("network edit cancellation and rejected saves preserve the current network and draft", async () => {
  const { fixture, detail, close } = await vmFixture();
  try {
    await detail
      .getByRole("button", { name: "Edit MicroNetwork", exact: true })
      .click();
    await detail
      .getByLabel("MicroNetwork", { exact: true })
      .selectOption("network-2");
    await detail.getByRole("button", { name: "Cancel", exact: true }).click();
    assert.equal(
      fixture.requests.filter((request) => request.method === "PUT").length,
      0,
    );
    await expect(detail).toContainText("default (10.42.0.0/16)");
    fixture.handlers.set(
      `PUT /api/vms/${fixture.state.vms[0].id}`,
      ({ send }) =>
        send(400, {
          error: {
            code: "validation_failed",
            message: "request validation failed",
            fields: { microNetworkId: "destination subnet is exhausted" },
          },
        }),
    );
    await detail
      .getByRole("button", { name: "Edit MicroNetwork", exact: true })
      .click();
    await detail
      .getByLabel("MicroNetwork", { exact: true })
      .selectOption("network-2");
    await detail.getByRole("button", { name: "Save", exact: true }).click();
    await expect(detail.locator(".prop-editor")).toContainText(
      "destination subnet is exhausted",
    );
    await expect(
      detail.getByLabel("MicroNetwork", { exact: true }),
    ).toHaveValue("network-2");
  } finally {
    await close();
  }
});

test("running and transitional VMs keep their MicroNetwork read-only", async () => {
  for (const state of ["running", "starting", "stopping", "paused"]) {
    const { detail, close } = await vmFixture(state);
    try {
      await expect(
        detail.getByRole("button", { name: "Edit MicroNetwork", exact: true }),
      ).toHaveCount(0);
      await expect(detail).toContainText("default (10.42.0.0/16)");
      await expect(detail.getByText("Stop to change").first()).toBeVisible();
    } finally {
      await close();
    }
  }
});

test("Korean mobile network tab retains the existing security group until an explicit selection", async () => {
  const { fixture, page, detail, errors, close } = await vmFixture("stopped", {
    locale: "ko",
    width: 390,
    securityGroups: [
      {
        id: "group-1",
        name: "original",
        microNetworkId: "network-1",
        rules: [],
      },
      {
        id: "group-2",
        name: "destination",
        microNetworkId: "network-2",
        rules: [],
      },
    ],
  });
  try {
    fixture.handlers.set(
      `PUT /api/vms/${fixture.state.vms[0].id}`,
      ({ body, send }) => {
        if (!body.securityGroupId) {
          send(400, {
            error: {
              code: "validation_failed",
              message: "request validation failed",
              fields: { securityGroupId: "select a compatible security group" },
            },
          });
          return;
        }
        Object.assign(fixture.state.vms[0], body);
        send(200, fixture.state.vms[0]);
      },
    );
    await detail.getByRole("tab", { name: "네트워크", exact: true }).click();
    await detail
      .getByRole("button", { name: "MicroNetwork 편집", exact: true })
      .click();
    await detail
      .getByLabel("MicroNetwork", { exact: true })
      .selectOption("network-2");
    const groups = detail.getByRole("combobox", { name: "보안그룹", exact: true });
    await expect(groups).toHaveValue("group-1");
    await expect(groups.locator('option[value="group-1"]')).toBeDisabled();
    await expect(detail).toContainText("기존 보안그룹을 자동 해제하지 않는다.");
    await detail.getByRole("button", { name: "저장", exact: true }).click();
    await expect(detail.locator(".prop-editor")).toContainText(
      "select a compatible security group",
    );
    assert.equal(
      fixture.requests.find((request) => request.method === "PUT").body
        .securityGroupId,
      undefined,
    );
    await groups.selectOption("group-2");
    await detail.getByRole("button", { name: "저장", exact: true }).click();
    await expect(detail).toContainText("private (10.50.0.0/24)");
    await expect(
      detail.getByRole("button", { name: "MicroNetwork 편집", exact: true }),
    ).toBeVisible();
    const saves = fixture.requests.filter(
      (request) => request.method === "PUT",
    );
    assert.equal(saves[1].body.securityGroupId, "group-2");
    assert.equal(saves[1].body.microNetworkId, "network-2");
    assert.deepEqual(errors, []);
    assert.ok(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    );
  } finally {
    await close();
  }
});
