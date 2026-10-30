import { expect, test, type Page } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const VMS = [
  vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running"),
  vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "stopped"),
  vm("sandbox-dev", "e7f8a9b0-c1d2-4e3f-a4b5-c6d7e8f9a0b1", "error"),
];

const HOST = {
  loadAverage1m: 0.42,
  memoryTotalMib: 65536,
  memoryAvailableMib: 24903,
  diskTotalGib: 1000,
  diskAvailableGib: 620,
  uptimeSeconds: 1036800,
  platform: { os: "macos", name: "macOS 26", version: null, architecture: "arm64", virtualization: "vz", system: "Ubuntu 24.04", kernel: "6.8" },
};

const nav = (page: Page) => page.getByRole("navigation", { name: "Main navigation" });
const hostCard = (page: Page) => page.getByRole("region", { name: "Host status" });

test.describe("dashboard frame", () => {
  test("uses the redesign tokens", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(page.getByText("api-gateway", { exact: true })).toBeVisible();

    const body = page.locator("body");
    await expect(body).toHaveCSS("font-family", /Pretendard/);
    await expect(body).toHaveCSS("background-color", "rgb(255, 255, 255)");
    await expect(page.locator("#vm-list-add")).toHaveCSS("background-color", "rgb(24, 24, 27)");
  });

  test("uses the cleanup tokens and flat primitives", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(page.getByText("api-gateway", { exact: true })).toBeVisible();
    const tokens = await page.evaluate(() => {
      const style = getComputedStyle(document.documentElement);
      return ["--border", "--border-soft", "--border-strong", "--bg-active", "--text-4", "--text-5"]
        .map((name) => style.getPropertyValue(name).trim());
    });
    expect(tokens).toEqual(["#ececef", "#f2f2f4", "#e4e4e8", "#ebebee", "#a1a1aa", "#c4c4ca"]);
    await expect(page.locator("#vm-list-add")).toHaveCSS("box-shadow", "none");
    const status = page.locator(".vm-status-label").first();
    await expect(status).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
    await expect(status).toHaveCSS("border-top-width", "0px");
    expect(await status.evaluate((el) => getComputedStyle(el, "::before").width)).toBe("7px");
  });

  test("picks memory with chips and keeps advanced fields folded", async ({ page }) => {
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    await expect(page.locator(".form-section h3")).toHaveText(["Basics", "Resources", "Network"]);
    const memory = page.getByRole("radiogroup", { name: "Memory" });
    await expect(memory.getByRole("radio")).toHaveCount(9);
    await memory.getByRole("radio", { name: "4 GiB" }).check();
    await expect(page.locator("#form-summary")).toContainText("4 GiB");
    await page.keyboard.press("ArrowRight");
    await expect(memory.getByRole("radio", { name: "8 GiB" })).toBeChecked();
    await expect(page.getByRole("radiogroup", { name: "Internet access" }).getByRole("radio", { name: "Allow internet" })).toBeChecked();
    await expect(page.locator("details.form-advanced")).not.toHaveAttribute("open", "");
    await expect(page.getByLabel("Environment", { exact: true })).toBeHidden();
    await expect(page.getByRole("button", { name: "Create MicroVM" })).toBeVisible();
  });

  test("the New MicroVM page has one heading, a head line and no button to itself", async ({ page }) => {
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    await expect(page.locator("#content h2")).toHaveText("New MicroVM");
    await expect(page.locator(".form-page > header p")).toHaveText("Pick an image and resources. The MicroVM starts in a few seconds.");
    // The header points at the page it is on, so it has no such button; the list keeps it.
    const header = page.locator(".screen-header");
    await expect(header.getByRole("link", { name: "New MicroVM" })).toHaveCount(0);
    await expect(header.getByRole("button", { name: "New MicroVM" })).toHaveCount(0);
    await page.goto("/#/vms");
    await expect(header.getByRole("link", { name: "New MicroVM" })).toBeVisible();
  });

  test("the New MicroVM page lays its sections out in a 760px column", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    const content = (await page.locator("#content").boundingBox())!;
    const sections = page.locator(".form-section");
    await expect(sections).toHaveCount(3);
    for (const box of await sections.evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().toJSON()))) {
      expect(box.width).toBeCloseTo(760, 0);
      expect(box.left + box.width / 2).toBeCloseTo(content.x + content.width / 2, 0);
    }
    // The title and its line sit left of the inputs; memory chips take the whole row.
    const head = (await sections.nth(1).locator(".form-section-head").boundingBox())!;
    const fields = (await sections.nth(1).locator(".form-section-fields").boundingBox())!;
    expect(head.width).toBeCloseTo(190, 0);
    expect(fields.x).toBeGreaterThan(head.x + head.width);
    const cpu = (await page.getByLabel("vCPU", { exact: true }).boundingBox())!;
    const disk = (await page.getByLabel("Disk", { exact: true }).boundingBox())!;
    expect(Math.abs(cpu.y - disk.y)).toBeLessThan(2);
    expect(disk.x).toBeGreaterThan(cpu.x + cpu.width);
    const name = (await page.getByLabel("Name", { exact: true }).boundingBox())!;
    const image = (await page.getByLabel("Image", { exact: true }).boundingBox())!;
    expect(Math.abs(name.y - image.y)).toBeLessThan(2);
    // The bar spans the page: summary at its left, Cancel and Create at its right.
    const bar = (await page.locator("#editor .form-footer").boundingBox())!;
    expect(bar.width).toBeCloseTo(content.width, 0);
    const summary = (await page.locator("#form-summary").boundingBox())!;
    const create = (await page.getByRole("button", { name: "Create MicroVM" }).boundingBox())!;
    expect(summary.x).toBeLessThan(bar.x + 40);
    expect(create.x + create.width).toBeGreaterThan(bar.x + bar.width - 40);
    await expect(page.locator("#editor .form-footer button").first()).toHaveClass(/ghost/);
    await expect(page.getByRole("button", { name: "Create MicroVM" })).toHaveClass(/primary/);
  });

  test("the New MicroVM footer is at the bottom of a page that is shorter than the screen", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1500 });
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    await expect(page.locator("#editor .form-footer")).toBeVisible();
    expect(await page.locator("#content").evaluate((el) => el.scrollHeight - el.clientHeight)).toBe(0);
    const gap = await page.evaluate(() => document.querySelector("#content")!.getBoundingClientRect().bottom - document.querySelector("#editor .form-footer")!.getBoundingClientRect().bottom);
    expect(Math.abs(gap)).toBeLessThanOrEqual(1);
  });

  test("the New MicroVM summary follows the image, vCPU, memory and disk", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/vms/new",
      routes: {
        "/api/images": [{ alias: "alpine", installed: true }, { alias: "traefik/whoami:latest", installed: true }],
        "/api/micro-networks": [], "/api/storage": [], "/api/shells": [],
      },
    });
    const summary = page.locator("#form-summary");
    await expect(summary).toHaveText("2 vCPU · 1 GiB · 8 GB · alpine");
    await page.getByLabel("Image", { exact: true }).selectOption("traefik/whoami:latest");
    await expect(summary).toHaveText("2 vCPU · 1 GiB · 8 GB · traefik/whoami:latest");
    await page.getByLabel("vCPU", { exact: true }).fill("4");
    await page.getByLabel("Disk", { exact: true }).fill("16");
    await page.getByRole("radiogroup", { name: "Memory" }).getByRole("radio", { name: "512 MiB" }).check();
    await expect(summary).toHaveText("4 vCPU · 512 MiB · 16 GB · traefik/whoami:latest");
    await page.getByLabel("vCPU", { exact: true }).fill("");
    await expect(summary).toHaveText("– vCPU · 512 MiB · 16 GB · traefik/whoami:latest");
  });

  test("the New MicroVM page shows the free host memory under the memory sizes", async ({ page }) => {
    await openDashboard(page, {
      hash: "#/vms/new",
      routes: { "/api/host": { memoryTotalMib: 8192, memoryAvailableMib: 2867 }, "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] },
    });
    const hint = page.locator("#hint-ram");
    await expect(hint).toHaveText("Free host memory 2.8 GiB");
    const group = (await page.getByRole("radiogroup", { name: "Memory" }).boundingBox())!;
    const line = (await hint.boundingBox())!;
    expect(line.y).toBeGreaterThan(group.y + group.height);
    await expect(page.getByRole("radiogroup", { name: "Memory" })).toHaveAccessibleDescription("Free host memory 2.8 GiB");
  });

  for (const [name, host] of [
    ["cannot be read", undefined],
    ["answers without free memory", { memoryTotalMib: 8192, loadAverage1m: 0.4 }],
    ["answers a free memory that is not a number", { memoryTotalMib: 8192, memoryAvailableMib: "lots" }],
  ] as const) {
    test(`the New MicroVM page shows no memory hint when the host ${name}`, async ({ page }) => {
      let asked = 0;
      await openDashboard(page, {
        hash: "#/vms/new",
        routes: {
          "/api/host": async (route) => {
            asked++;
            if (host) await route.fulfill({ json: host });
            else await route.fulfill({ status: 500, json: { error: { message: "host unavailable" } } });
          },
          "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [],
        },
      });
      await expect(page.getByRole("radiogroup", { name: "Memory" })).toBeVisible();
      await expect.poll(() => asked).toBeGreaterThan(0);
      await page.waitForTimeout(300);
      await expect(page.locator("#hint-ram")).toBeEmpty();
      await expect(page.locator(".field-hint:visible")).toHaveCount(0);
      await expect(page.getByRole("alert")).toHaveCount(0);
    });
  }

  test("the New MicroVM page does not wait for the host, and keeps what is typed while it answers", async ({ page }) => {
    let answer!: () => void;
    const answered = new Promise<void>((resolve) => { answer = resolve; });
    await openDashboard(page, {
      hash: "#/vms/new",
      routes: {
        "/api/host": async (route) => { await answered; await route.fulfill({ json: { memoryTotalMib: 8192, memoryAvailableMib: 2867 } }); },
        "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [],
      },
    });
    // The form is there while the host has not answered, and the hint line takes no room yet.
    const name = page.getByLabel("Name", { exact: true });
    await name.fill("typed-first");
    await expect(page.locator("#hint-ram")).toBeEmpty();
    expect(await page.locator("#hint-ram").evaluate((el) => getComputedStyle(el).display)).toBe("none");
    answer();
    await expect(page.locator("#hint-ram")).toHaveText("Free host memory 2.8 GiB");
    // The line was filled in place: the typed value and the focus stay.
    await expect(name).toHaveValue("typed-first");
    await expect(name).toBeFocused();
  });

  test("the New MicroVM page uses the host it already polled", async ({ page }) => {
    let calls = 0;
    await openDashboard(page, {
      routes: {
        "/api/host": async (route) => {
          // Only the first answer is quick; a request made for the create page would hang.
          if (++calls > 1) await new Promise((resolve) => setTimeout(resolve, 5000));
          await route.fulfill({ json: HOST }).catch(() => {});
        },
        "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [],
      },
    });
    await expect(hostCard(page)).toContainText("62%");
    await page.locator(".screen-header").getByRole("link", { name: "New MicroVM" }).click();
    await expect(page.locator("#hint-ram")).toHaveText("Free host memory 24.3 GiB", { timeout: 1500 });
  });

  test("the New MicroVM page names its fields as the design does", async ({ page }) => {
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    for (const label of ["Name", "Image", "vCPU", "Disk", "MicroNetwork", "Port forwarding"]) {
      await expect(page.getByLabel(label, { exact: true })).toBeVisible();
    }
    await expect(page.getByLabel("CPU", { exact: true })).toHaveCount(0);
    await expect(page.getByLabel("Disk (GB)", { exact: true })).toHaveCount(0);
    await expect(page.locator("details.form-advanced summary")).toContainText("Storage · Shells · Environment · Data volumes");
    // Examples sit in the empty boxes.
    await expect(page.getByLabel("Port forwarding", { exact: true })).toHaveAttribute("placeholder", "e.g. 8080:80/tcp");
    await page.locator("details.form-advanced summary").click();
    await expect(page.getByLabel("Environment", { exact: true })).toHaveAttribute("placeholder", "NAME=value");
    // The disk unit sits inside the box, at its right, and the value stops before it.
    const disk = page.getByLabel("Disk", { exact: true });
    const box = (await disk.boundingBox())!;
    const unit = page.locator("#unit-diskGb");
    await expect(unit).toHaveText("GB");
    const mark = (await unit.boundingBox())!;
    expect(mark.x).toBeGreaterThan(box.x + box.width / 2);
    expect(mark.x + mark.width).toBeLessThanOrEqual(box.x + box.width);
    expect(mark.y).toBeGreaterThanOrEqual(box.y);
    expect(mark.y + mark.height).toBeLessThanOrEqual(box.y + box.height);
    expect(await disk.evaluate((el) => parseFloat(getComputedStyle(el).paddingRight))).toBeGreaterThanOrEqual(mark.width + 11);
    await expect(disk).toHaveAccessibleDescription("GB");
    await disk.fill("123456");
    await expect(page.locator("#form-summary")).toContainText("123456 GB");
    // English vCPU has no unit; the box stays as wide as the disk box.
    await expect(page.locator("#unit-cpu")).toHaveCount(0);
    expect((await page.getByLabel("vCPU", { exact: true }).boundingBox())!.width).toBeCloseTo(box.width, 0);
    // Port forwarding is about one line high.
    expect((await page.getByLabel("Port forwarding", { exact: true }).boundingBox())!.height).toBeLessThan(44);
  });

  test("the New MicroVM page reads in Korean", async ({ page }) => {
    await openDashboard(page, {
      locale: "ko",
      hash: "#/vms/new",
      routes: { "/api/host": { memoryTotalMib: 8192, memoryAvailableMib: 2867 }, "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] },
    });
    await expect(page.locator("#content h2")).toHaveText("새 MicroVM");
    await expect(page.locator(".form-page > header p")).toHaveText("이미지와 사양을 고르면 몇 초 안에 시작합니다.");
    await expect(page.locator(".form-section h3")).toHaveText(["기본", "사양", "네트워크"]);
    await expect(page.locator(".form-section-head p")).toHaveText([
      "이름과 시작할 이미지를 정합니다.",
      "이 MicroVM의 vCPU, 메모리, 디스크입니다.",
      "격리하면 같은 MicroNetwork 안에서만 통신합니다.",
    ]);
    await expect(page.locator("#hint-ram")).toHaveText("호스트 여유 메모리 2.8 GiB");
    await expect(page.getByRole("radiogroup", { name: "메모리" }).getByRole("radio", { name: "1 GiB" })).toBeChecked();
    await expect(page.getByRole("radio", { name: "인터넷 허용" })).toBeChecked();
    await expect(page.getByRole("radiogroup", { name: "외부 통신" })).toBeVisible();
    for (const label of ["vCPU", "디스크", "MicroNetwork", "포트 포워딩"]) {
      await expect(page.getByLabel(label, { exact: true })).toBeVisible();
    }
    await expect(page.locator("#unit-cpu")).toHaveText("개");
    await expect(page.locator("#unit-diskGb")).toHaveText("GB");
    await expect(page.getByLabel("포트 포워딩", { exact: true })).toHaveAttribute("placeholder", "예: 8080:80/tcp");
    await expect(page.locator("details.form-advanced summary")).toContainText("고급 설정");
    await expect(page.locator(".form-advanced-list")).toHaveText("스토리지 · Shell · 환경 변수 · 데이터 볼륨");
    await expect(page.getByRole("button", { name: "MicroVM 만들기" })).toBeVisible();
    await expect(page.getByRole("button", { name: "취소" })).toBeVisible();
    await expect(page.locator(".screen-header").getByRole("link", { name: "새 MicroVM" })).toHaveCount(0);
  });

  test("the New MicroVM page fits a phone", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [{ alias: "traefik/whoami:latest", installed: true }], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    await expect(page.getByRole("button", { name: "Create MicroVM" })).toBeInViewport();
    await expect(page.getByRole("button", { name: "Cancel" })).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(await page.locator("#content").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    // Sections stack: the title above its inputs.
    const head = (await page.locator(".form-section-head").first().boundingBox())!;
    const fields = (await page.locator(".form-section-fields").first().boundingBox())!;
    expect(fields.y).toBeGreaterThan(head.y + head.height - 1);
    expect(head.x).toBeGreaterThanOrEqual(15);
  });

  test("the New MicroVM form keeps its footer at the bottom of the page", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 560 });
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    const footer = page.locator("#editor .form-footer");
    await expect(footer.locator("button").first()).toHaveText("Cancel");
    const content = page.locator("#content");
    expect(await content.evaluate((el) => el.scrollHeight - el.clientHeight)).toBeGreaterThan(0);
    for (const top of [0, 200, 100000]) {
      await content.evaluate((el, value) => { el.scrollTop = value; }, top);
      const gap = await page.evaluate(() => document.querySelector("#content")!.getBoundingClientRect().bottom - document.querySelector("#editor .form-footer")!.getBoundingClientRect().bottom);
      expect(Math.abs(gap)).toBeLessThanOrEqual(1);
    }
  });

  test("groups the navigation", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    for (const group of ["Compute", "Network", "Data", "System", "AI"]) {
      await expect(nav(page).getByText(group, { exact: true })).toBeVisible();
    }
    for (const name of ["MicroVM", "Pools", "MicroService", "Schedules", "Networks", "Security groups", "Pipe", "Secrets", "Storage", "Images", "Kernels", "Shells", "Host"]) {
      await expect(nav(page).getByRole("link", { name, exact: true })).toBeVisible();
    }
  });

  test("shows the MicroVM count and error badge", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    const item = nav(page).getByRole("link", { name: "MicroVM", exact: true });
    await expect(item.locator(".sidebar-count")).toHaveText("3");
    await expect(item.locator(".sidebar-alert")).toHaveText("1");
  });

  test("shows host status and falls back when it fails", async ({ page }) => {
    await openDashboard(page, { vms: VMS, routes: { "/api/host": HOST } });
    await expect(hostCard(page)).toContainText("0.42");
    await expect(hostCard(page)).toContainText("62%");
    await expect(hostCard(page)).toContainText("38%");
    await expect(nav(page)).toContainText("macOS 26 · arm64");
  });

  test("shows dashes when host status cannot be read", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(hostCard(page).locator(".host-meter-value")).toHaveText(["—", "—", "—"]);
  });

  test("keeps the sidebar to menus and a compact host status", async ({ page }) => {
    await openDashboard(page, { vms: VMS, routes: { "/api/host": HOST } });
    const items = nav(page).locator(".sidebar-item");
    await expect(items).toHaveCount(19); // one per VIEWS entry in src/model.rs
    await expect(nav(page).getByRole("link", { name: "Stack", exact: true })).toHaveAttribute("href", "#/stacks");
    await expect(nav(page).getByRole("link", { name: "Security", exact: true })).toHaveAttribute("href", "#/security");
    for (const item of await items.all()) await expect(item.locator("svg")).toHaveCount(1);
    await expect(nav(page).locator(".sidebar-item.active")).toHaveCSS("box-shadow", "none");
    await expect(nav(page).locator(".sidebar-search")).toHaveCSS("box-shadow", "none");
    await expect(nav(page).getByRole("link", { name: "Host and updates" })).toHaveCount(0);
    await expect(page.locator("#update-indicator")).toBeEmpty();
    await expect(hostCard(page)).toContainText("Host healthy");
    await hostCard(page).getByRole("link").click();
    await expect(page).toHaveURL(/#\/host$/);
  });

  test("keeps keyboard focus on the host status link while the status refreshes", async ({ page }) => {
    let load = 0.42;
    await openDashboard(page, {
      vms: VMS,
      routes: { "/api/host": (route) => route.fulfill({ json: { ...HOST, loadAverage1m: load } }) },
    });
    const link = hostCard(page).getByRole("link");
    await expect(page.locator("#host-load")).toHaveText("0.42");
    await link.focus();
    await expect(link).toBeFocused();
    load = 1.5; // the next 3s poll brings a new value
    await expect(page.locator("#host-load")).toHaveText("1.50");
    await expect(link).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/#\/host$/);
  });

  test("turns the host status healthy in place once the host answers", async ({ page }) => {
    let up = false;
    await openDashboard(page, {
      vms: VMS,
      routes: { "/api/host": (route) => (up ? route.fulfill({ json: HOST }) : route.fulfill({ status: 500, json: { error: { message: "down" } } })) },
    });
    await expect(hostCard(page)).toContainText("Host status unavailable");
    await expect(page.locator("#host-dot")).not.toHaveClass(/\bok\b/);
    up = true;
    await expect(hostCard(page)).toContainText("Host healthy");
    await expect(page.locator("#host-dot")).toHaveClass(/\bok\b/);
    await expect(page.locator("#host-load")).toHaveText("0.42");
  });

  test("shows the frame status text in Korean", async ({ page }) => {
    await openDashboard(page, { vms: VMS, locale: "ko", routes: { "/api/host": HOST } });
    await expect(hostCard(page)).toContainText("호스트 정상");
    await expect(hostCard(page)).toContainText("메모리");
    await expect(page.locator("#api-status")).toHaveCount(0);
  });

  test("puts the banner under the screen header", async ({ page }) => {
    await openDashboard(page, {
      routes: { "/api/vms": (route) => route.fulfill({ status: 500, json: { error: { message: "VM list unavailable" } } }) },
    });
    const header = page.locator(".screen-header");
    const banner = page.getByRole("alert").filter({ hasText: "VM list unavailable" });
    await expect(banner).toBeVisible();
    await expect(page.locator(".screen-header + #messages [role=alert]")).toHaveCount(1);
    await expect(header).not.toContainText("API unavailable");
  });

  test("keeps the header free of connection status, refresh and panel toggle buttons", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(page.locator("#api-status")).toHaveCount(0);
    for (const hash of ["#/vms", "#/networks", "#/secrets", "#/pipe", "#/host"]) {
      await page.goto(`/${hash}`);
      const header = page.locator(".screen-header");
      await expect(header.locator("#api-status")).toHaveCount(0);
      await expect(header.getByRole("button", { name: "Refresh", exact: true })).toHaveCount(0);
      await expect(header.getByRole("button", { name: /^(Show details|Hide details|Details)$/ })).toHaveCount(0);
    }
  });

  test("shows the parent crumb on the create page", async ({ page }) => {
    await openDashboard(page, { hash: "#/vms/new", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    await expect(page.locator(".screen-parent")).toHaveText("MicroVM");
    await expect(page.locator(".screen-heading h1")).toHaveText("New MicroVM");
  });

  test("keeps API errors in the banner and recovers by polling", async ({ page }) => {
    let unavailable = true;
    await openDashboard(page, {
      routes: { "/api/vms": (route) => unavailable
        ? route.fulfill({ status: 500, json: { error: { message: "VM list unavailable" } } })
        : route.fulfill({ json: VMS }) },
    });
    await expect(page.getByRole("alert")).toContainText("VM list unavailable");
    await expect(page.locator("#api-status")).toHaveCount(0);
    unavailable = false;
    await expect(page.getByRole("button", { name: "api-gateway", exact: true })).toBeVisible();
    await expect(page.getByRole("alert")).toHaveCount(0);
  });

  test("keeps the language select", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await expect(page.locator(".screen-header")).toContainText("컴퓨팅");
  });

  test("uses a drawer on small screens", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await openDashboard(page, { vms: VMS });
    await expect(nav(page)).toBeHidden();
    await page.getByRole("button", { name: "Open menu", exact: true }).click();
    await expect(nav(page)).toBeVisible();
    await nav(page).getByRole("link", { name: "Networks", exact: true }).click();
    await expect(page).toHaveURL(/#\/networks$/);
    await expect(nav(page)).toBeHidden();
  });

  test("the console terminal starts with the mono token font", async ({ page }) => {
    const target = VMS[0];
    await openDashboard(page, { vms: VMS, hash: `#/console/${target.id}`, routes: { [`/api/vms/${target.id}`]: target } });
    const rows = page.locator(".console-page .terminal-emulator");
    await expect(rows).toBeAttached();
    await expect(rows).toHaveCSS("font-family", /^"JetBrains Mono"/);
  });

  test("clears search and state filters without losing keyboard focus", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(page.locator("#vm-result-count")).toHaveText("3 of 3 MicroVMs");
    await page.getByRole("tab", { name: /^Running/ }).click();
    const search = page.getByRole("searchbox", { name: "Filter MicroVMs" });
    await search.fill("missing-instance");
    await expect(page.locator("#vm-result-count")).toHaveText("0 of 3 MicroVMs");
    await page.locator("#vm-empty").getByRole("button", { name: "Clear filters" }).click();
    await expect(search).toHaveValue("");
    await expect(search).toBeFocused();
    await expect(page.getByRole("tab", { name: /^All/ })).toHaveAttribute("aria-selected", "true");
    await expect(page.locator("#vm-result-count")).toHaveText("3 of 3 MicroVMs");
    await expect(page.locator("#vm-empty")).toBeHidden();
  });

  test("guides an empty workspace to its first MicroVM", async ({ page }) => {
    await openDashboard(page, { routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    const empty = page.locator("#vm-empty");
    await expect(empty).toContainText("Create your first MicroVM");
    await expect(empty).toContainText("Pick an image and resources. It starts in a few seconds.");
    await expect(empty.locator(".empty-state-icon svg")).toBeVisible();
    await empty.getByRole("link", { name: "New MicroVM" }).click();
    await expect(page).toHaveURL(/#\/vms\/new$/);
    await expect(page.getByRole("radiogroup", { name: "Memory" })).toBeVisible();
  });

  test("shows the empty workspace and the filter miss in Korean", async ({ page }) => {
    await openDashboard(page, { locale: "ko", routes: { "/api/images": [], "/api/micro-networks": [], "/api/storage": [], "/api/shells": [] } });
    const empty = page.locator("#vm-empty");
    await expect(empty.locator(".empty-state-title")).toHaveText("첫 MicroVM을 만듭니다");
    await expect(empty).toContainText("이미지와 사양을 고르면 몇 초 안에 시작합니다.");
    await expect(empty.getByRole("link", { name: "새 MicroVM" })).toHaveAttribute("href", "#/vms/new");
    await expect(page.locator("#vm-summary svg")).toHaveCount(0);

    // A filter that matches nothing, with VMs present.
    await openDashboard(page, { locale: "ko", vms: VMS });
    await page.getByRole("searchbox", { name: "MicroVM 필터" }).fill("missing-instance");
    await expect(empty.locator(".empty-state-title")).toHaveText("검색 결과 없음");
    await expect(empty).toContainText("조건에 맞는 MicroVM이 없습니다.");
    await expect(empty.getByRole("button", { name: "필터 초기화" })).toBeVisible();
    await expect(empty.getByRole("link", { name: "새 MicroVM" })).toHaveCount(0);
  });

  test("supports arrow and boundary keys in state tabs", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    const all = page.getByRole("tab", { name: /^All/ });
    const running = page.getByRole("tab", { name: /^Running/ });
    await all.focus();
    await page.keyboard.press("ArrowRight");
    await expect(running).toBeFocused();
    await expect(running).toHaveAttribute("aria-selected", "true");
    await expect(page.locator("#vm-result-count")).toHaveText("1 of 3 MicroVMs");
    await page.keyboard.press("End");
    await expect(page.getByRole("tab", { name: /^Error/ })).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(all).toBeFocused();
    await page.keyboard.press("End");
    await page.keyboard.press("Home");
    await expect(all).toBeFocused();
    await expect(page.locator("#vm-result-count")).toHaveText("3 of 3 MicroVMs");
  });

  test("contains mobile menu focus and restores it on dismissal", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await openDashboard(page, { vms: VMS });
    const toggle = page.getByRole("button", { name: "Open menu", exact: true });
    await toggle.click();
    await expect(nav(page).getByRole("link", { name: "MicroVM", exact: true })).toBeFocused();
    await expect(page.locator("#nav-toggle")).toHaveAttribute("aria-expanded", "true");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Escape");
    await expect(page.getByRole("complementary", { name: "MicroVM detail" })).toBeHidden();
    await toggle.click();
    await page.getByRole("combobox", { name: "Language", exact: true }).focus();
    await page.keyboard.press("Tab");
    await expect(page.locator("#nav-toggle")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(nav(page)).toBeHidden();
    await expect(toggle).toBeFocused();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await toggle.click();
    await page.locator(".nav-backdrop").click({ position: { x: 350, y: 200 } });
    await expect(nav(page)).toBeHidden();
    await toggle.click();
    await page.setViewportSize({ width: 1440, height: 900 });
    await expect(page.locator(".app-workspace")).not.toHaveAttribute("inert", "");
    await expect(nav(page)).toBeVisible();
  });

  test("fits the mobile header and overview without page overflow", async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 800 });
    await openDashboard(page, { vms: VMS, locale: "ko" });
    await expect(page.locator(".vm-overview")).toHaveCount(0);
    const menu = await page.getByRole("button", { name: "메뉴 열기", exact: true }).boundingBox();
    const heading = await page.locator(".screen-heading").boundingBox();
    expect(menu!.x + menu!.width).toBeLessThanOrEqual(heading!.x);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(360);
    await expect(page.locator("#vm-result-count")).toHaveText("전체 3개 중 3개 표시");
  });
});
