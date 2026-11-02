import { expect, test } from "@playwright/test";

const vmId = "33333333-3333-4333-8333-333333333333";

for (const locale of ["en", "ko"] as const)
  for (const retiredLink of [false, true])
    test(`${locale} Sandbox opens quick start without creating a workspace${retiredLink ? " from an old model-task link" : ""} @dashboard`, async ({
      page,
    }) => {
      const obsoleteRequests: string[] = [];
      await page.addInitScript(
        (value) => localStorage.setItem("firecrab.locale", value),
        locale,
      );
      await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
        const path = new URL(route.request().url()).pathname;
        if (
          path.startsWith("/api/ai-sandbox") ||
          path.startsWith("/api/pipe/ai")
        ) {
          obsoleteRequests.push(path);
          return route.fulfill({
            status: 410,
            json: {
              error: {
                message: "Retired project UI must not request this endpoint",
              },
            },
          });
        }
        if (path === "/api/vms")
          return route.fulfill({
            json: [
              {
                id: vmId,
                name: "coding-vm",
                template: "nginx-1.28",
                state: "running",
                purpose: "instance",
                cpu: 1,
                ram: 1024,
                diskGb: 3,
                usageHistory: [],
                startupTimeline: [],
                env: {},
                shellRefs: [],
                portForwards: [],
              },
            ],
          });
        if (path === "/api/host")
          return route.fulfill({
            json: {
              uptimeSeconds: 60,
              loadAverage1m: 1,
              memoryTotalMib: 4096,
              memoryAvailableMib: 2048,
              diskTotalGib: 32,
              diskAvailableGib: 16,
              platform: {
                os: "macos",
                name: "macOS",
                architecture: "arm64",
                virtualization: "Virtualization.framework",
                controllerId: "11111111-1111-4111-8111-111111111111",
              },
            },
          });
        if (path === "/api/update")
          return route.fulfill({
            json: { current: "0.4.1", latest: "0.4.1", updateAvailable: false },
          });
        return route.fulfill({ json: [] });
      });
      await page.goto(
        `/#/ai-sandbox${retiredLink ? "?view=models&project=old-project" : ""}`,
      );
      await expect(
        page.getByRole("heading", { name: "AI Sandbox", exact: true }),
      ).toBeVisible();
      await expect(
        page.getByRole("combobox", { name: "MicroVM", exact: true }),
      ).toHaveValue("");
      await expect(
        page.getByRole("heading", {
          name:
            locale === "ko"
              ? "새 작업 공간"
              : "New workspace",
          exact: true,
        }),
      ).toBeVisible();
      await expect(
        page.getByRole("button", {
          name: locale === "ko" ? "작업 공간 만들기" : "Create workspace",
          exact: true,
        }),
      ).toBeEnabled();
      await expect(
        page.getByRole("button", {
          name: locale === "ko" ? "새 MicroVM" : "New MicroVM",
          exact: true,
        }),
      ).toBeEnabled();
      await expect(page.getByRole("combobox", { name: "MicroVM", exact: true })).toHaveValue("");
      await expect(page.getByText("Ubuntu 26.04 LTS", { exact: true })).toBeVisible();
      await expect(page.getByRole("button", { name: locale === "ko" ? "작업 공간 만들기" : "Create workspace", exact: true })).toBeEnabled();
      for (const name of [
        "Model tasks",
        "모델 작업",
        "New project",
        "새 프로젝트",
        "Project settings",
        "프로젝트 설정",
        "Workspace options",
        "작업 공간 메뉴",
      ])
        await expect(
          page.getByRole("button", { name, exact: true }),
        ).toHaveCount(0);
      await expect(page.getByRole("dialog")).toHaveCount(0);
      expect(obsoleteRequests).toEqual([]);
    });
