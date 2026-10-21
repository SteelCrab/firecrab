import { expect, test } from "@playwright/test";

for (const os of ["macos", "windows"] as const) {
  test(`${os} Host saves allocations and tracks restart completion @dashboard`, async ({ page }, testInfo) => {
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    let controllerId = "test-host";
    const initial = { cpu: 2, memoryMib: 4096, diskGib: os === "macos" ? 16 : 1024, networkMode: "nat", networkInterface: null };
    let settings = { ...initial };
    let current = { ...initial };
    let operation: null | { id: string; state: string; error: string | null } = { id: "previous-apply", state: "failed", error: "Previous apply failed" };
    let resourcePolls = 0;
    let applyCount = 0;
    await page.route(/http:\/\/[^/]+\/api\//, async route => {
      const request = route.request();
      const url = new URL(request.url());
      if (url.pathname === "/api/service/resources/apply") {
        expect(request.headers()["x-firecrab-controller-id"]).toBe("test-host");
        operation = { id: `test-apply-${++applyCount}`, state: "applying", error: null };
        if (os === "macos") await route.abort("connectionreset");
        else await route.fulfill({ status: 202, json: operation });
        return;
      }
      if (url.pathname === "/api/service/resources") {
        if (request.method() === "PUT") {
          expect(request.headers()["x-firecrab-controller-id"]).toBe("test-host");
          settings = request.postDataJSON();
        } else resourcePolls += 1;
        await route.fulfill({ json: { controllerId, os, settings, current, pending: JSON.stringify(settings) !== JSON.stringify(current) || operation?.state === "failed", sharedWsl: os === "windows", host: { cpu: 8, memoryMib: 16384, diskAvailableGib: 200 + resourcePolls, networkInterfaces: ["en0", "en1"], networkModes: ["nat", "bridged", "mirrored"] }, operation } });
        return;
      }
      if (url.pathname === "/api/host") {
        await route.fulfill({ json: { loadAverage1m: 0.2, memoryTotalMib: 4096, memoryAvailableMib: 2048, diskTotalGib: initial.diskGib, diskAvailableGib: 10, uptimeSeconds: 3600, platform: { controllerId: "test-host", os, name: os === "macos" ? "macOS" : "Windows 11", architecture: "arm64", version: "26", virtualization: os === "macos" ? "Virtualization.framework" : "WSL2", system: "Debian 13", kernel: "6.12" } } });
        return;
      }
      if (url.pathname === "/api/network") {
        await route.fulfill({ json: { bridgeName: "fcbr0", subnetCidr: "172.16.0.0/24", gateway: "172.16.0.1", uplink: "eth0" } });
        return;
      }
      await route.fulfill({ json: [] });
    });
    await page.goto("/#/host");
    await expect(page.getByLabel("CPU (vCPU)")).toHaveValue("2");
    await expect(page.getByText("Previous apply failed", { exact: true })).toHaveCount(0);
    await page.getByLabel("CPU (vCPU)").fill("4");
    await page.getByLabel("RAM (MiB)").fill("8192");
    await page.getByLabel("Network", { exact: true }).selectOption(os === "macos" ? "bridged" : "mirrored");
    if (os === "macos") await expect(page.getByLabel("Physical interface")).toHaveValue("en0");
    else await expect(page.getByText("CPU, RAM, and networking affect every WSL2 distribution.", { exact: false })).toBeVisible();
    await page.getByRole("button", { name: "Save settings" }).click();
    await expect(page.getByRole("button", { name: "Apply and restart" })).toBeEnabled();
    expect(settings.cpu).toBe(4);
    expect(settings.memoryMib).toBe(8192);
    await page.getByRole("button", { name: "Apply and restart" }).click();
    await expect(page.getByRole("button", { name: "Applying…" })).toBeDisabled();
    operation = { id: "test-apply-1", state: "failed", error: "Current apply failed" };
    await expect(page.getByText("Current apply failed", { exact: true })).toBeVisible();
    await page.getByLabel("CPU (vCPU)").fill("6");
    await page.getByRole("button", { name: "Save settings" }).click();
    await expect(page.getByText("Saved. Apply to restart with these resources.")).toBeVisible();
    const nextPoll = resourcePolls + 1;
    await expect(page.getByText(new RegExp(`${200 + nextPoll} GiB disk available`))).toBeVisible();
    await expect(page.getByText("Current apply failed", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Apply and restart" }).click();
    await expect(page.getByRole("button", { name: "Applying…" })).toBeDisabled();
    current = { ...settings };
    operation = { id: "test-apply-2", state: "complete", error: null };
    await expect(page.getByText("Resources applied. The management VM is ready.")).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath(`firecrab-resources-${os}-desktop.png`), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: testInfo.outputPath(`firecrab-resources-${os}-mobile.png`), fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    controllerId = "different-machine";
    await expect(page.getByText("This controller belongs to a different host", { exact: false })).toBeVisible();
    await expect(page.getByLabel("CPU (vCPU)")).toHaveCount(0);
    expect(errors).toEqual([]);
  });
}
