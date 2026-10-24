import type { Page, Route } from "@playwright/test";
import type { VmResponse } from "../../../firecrab-frontend/src/bindings";

/** A VM record with every field the dashboard reads; `overrides` replaces any of them. */
export function vm(name: string, id: string, state: VmResponse["state"], overrides: Partial<VmResponse> = {}): VmResponse {
  return {
    id,
    name,
    state,
    reconciliation: null,
    template: "ubuntu-24.04",
    templateVersion: "2026.09.1",
    cpu: 2,
    ram: 1024,
    diskGb: 8,
    startupStep: null,
    startupTimeline: [],
    egressPolicy: "internet",
    ipv4: state === "running" ? "10.42.0.11" : null,
    ipv6: null,
    mac: null,
    hostname: name,
    microNetworkId: "default",
    storageRoot: "default",
    cpuUsagePercent: null,
    memoryUsedMib: null,
    memoryTotalMib: null,
    memoryUsedPercent: null,
    usageHistory: [],
    shellRefs: [],
    portForwards: [],
    env: {},
    sshHostFingerprint: null,
    ...overrides,
  };
}

/** A JSON body, or a handler that answers the request itself. */
export type MockRoute = object | ((route: Route) => Promise<void> | void);

export interface DashboardOptions {
  /** The VM list, or a function asked again on every poll. */
  vms?: VmResponse[] | (() => VmResponse[]);
  locale?: "en" | "ko";
  /** Answers by exact API path, e.g. `"/api/host"`. Unlisted paths get 404. */
  routes?: Record<string, MockRoute>;
  /** Hash route to open. */
  hash?: string;
}

/**
 * Opens the dashboard with every `/api/*` call answered in the browser, so the
 * suite runs against the Vite dev server alone — no Linux API or VM.
 */
export async function openDashboard(page: Page, options: DashboardOptions = {}): Promise<void> {
  const { vms = [], locale = "en", routes = {}, hash = "#/vms" } = options;
  await page.addInitScript((language) => localStorage.setItem("firecrab.locale", language), locale);
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const custom = routes[path];
    if (typeof custom === "function") return custom(route);
    if (custom) return route.fulfill({ json: custom });
    if (path === "/api/vms") return route.fulfill({ json: typeof vms === "function" ? vms() : vms });
    if (path === "/api/microblockstorage") return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "not mocked" } } });
  });
  await page.goto(`/${hash}`);
}
