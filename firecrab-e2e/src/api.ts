import { apiUrl, NETWORK_NAME, VM_NAME } from "./constants.js";

interface VmRow {
  id: string;
  name: string;
  state: string;
  template: string;
  ipv4?: string | null;
  ipv6?: string | null;
  portForwards?: Array<{ hostPort: number; guestPort: number; protocol: string }>;
}

interface ImageRow {
  alias: string;
  installed: boolean;
}

interface NetworkRow {
  id: string;
  name: string;
  ipv6Cidr?: string | null;
  ipv6Gateway?: string | null;
  ipv6AddressMode?: string | null;
  ipv6Egress?: string | null;
}

interface CatalogImageRow {
  alias: string;
  version: string;
}

export interface CatalogEntry {
  alias: string;
  version: string;
  downloadable: boolean;
  installed: boolean;
  packageStaged: boolean;
}

export interface RegisterSnapshot {
  alias: string;
  status: string;
  log: string;
}

export class ApiCleanup {
  constructor(private readonly base = apiUrl()) {}

  private async request(
    method: string,
    pathname: string,
    body?: unknown,
  ): Promise<{ status: number; json: unknown }> {
    const response = await fetch(`${this.base}${pathname}`, {
      method,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    let json: unknown = null;
    const text = await response.text();
    if (text) {
      try {
        json = JSON.parse(text);
      } catch {
        json = { raw: text };
      }
    }
    return { status: response.status, json };
  }

  async listVms(): Promise<VmRow[]> {
    const { status, json } = await this.request("GET", "/api/vms");
    if (status >= 400 || !Array.isArray(json)) return [];
    return json as VmRow[];
  }

  async getVm(id: string): Promise<VmRow | null> {
    const { status, json } = await this.request("GET", `/api/vms/${id}`);
    if (status >= 400 || !json || typeof json !== "object") return null;
    const row = json as VmRow;
    if (typeof row.id !== "string" || typeof row.name !== "string") return null;
    return row;
  }

  async listImages(): Promise<ImageRow[]> {
    const { status, json } = await this.request("GET", "/api/images");
    if (status >= 400 || !Array.isArray(json)) return [];
    return json as ImageRow[];
  }

  async listNetworks(): Promise<NetworkRow[]> {
    const { status, json } = await this.request("GET", "/api/micro-networks");
    if (status >= 400 || !Array.isArray(json)) return [];
    return json as NetworkRow[];
  }

  async listMicroregistry(): Promise<CatalogImageRow[]> {
    const { status, json } = await this.request("GET", "/api/microregistry");
    if (status >= 400 || !json || typeof json !== "object") return [];
    const images = (json as { images?: unknown }).images;
    if (!Array.isArray(images)) return [];
    const rows: CatalogImageRow[] = [];
    for (const image of images) {
      if (!image || typeof image !== "object") continue;
      const row = image as { alias?: unknown; version?: unknown };
      if (typeof row.alias !== "string" || typeof row.version !== "string") continue;
      rows.push({ alias: row.alias, version: row.version });
    }
    return rows;
  }

  async catalogEntry(alias: string): Promise<CatalogEntry | null> {
    const { status, json } = await this.request("GET", "/api/microregistry");
    if (status >= 400 || !json || typeof json !== "object") return null;
    const images = (json as { images?: unknown }).images;
    if (!Array.isArray(images)) return null;
    for (const image of images) {
      if (!image || typeof image !== "object") continue;
      const row = image as {
        alias?: unknown;
        version?: unknown;
        downloadable?: unknown;
        installed?: unknown;
        packageStaged?: unknown;
      };
      if (row.alias !== alias || typeof row.version !== "string") continue;
      return {
        alias,
        version: row.version,
        downloadable: row.downloadable === true,
        installed: row.installed === true,
        packageStaged: row.packageStaged === true,
      };
    }
    return null;
  }

  async startRegister(
    alias: string,
    version: string,
  ): Promise<{ status: number; json: unknown }> {
    return this.request("POST", "/api/microregistry/register", { alias, version });
  }

  async getRegister(alias: string): Promise<RegisterSnapshot | null> {
    const { status, json } = await this.request(
      "GET",
      `/api/microregistry/register/${encodeURIComponent(alias)}`,
    );
    if (status >= 400 || !json || typeof json !== "object") return null;
    const body = json as { alias?: unknown; status?: unknown; log?: unknown };
    if (typeof body.alias !== "string" || typeof body.status !== "string") return null;
    return {
      alias: body.alias,
      status: body.status,
      log: typeof body.log === "string" ? body.log : "",
    };
  }

  async deleteStagedPackage(alias: string): Promise<void> {
    await this.request("DELETE", `/api/images/${encodeURIComponent(alias)}/package`);
  }

  /**
   * Best-effort local catalog delete. L3 is insert-only today, so this
   * 404s until that layer grows a DELETE.
   */
  async deleteLocalCatalogRow(alias: string): Promise<boolean> {
    const encoded = encodeURIComponent(alias);
    for (const pathname of [
      `/api/microregistry/${encoded}`,
      `/api/microregistry/local/${encoded}`,
    ]) {
      const { status } = await this.request("DELETE", pathname);
      if (status === 200 || status === 204) return true;
    }
    return false;
  }

  async stopVm(id: string): Promise<void> {
    await this.request("POST", `/api/vms/${id}/stop`);
  }

  async deleteVm(id: string): Promise<void> {
    await this.request("DELETE", `/api/vms/${id}`);
  }

  async deleteImage(alias: string): Promise<void> {
    await this.request("DELETE", `/api/images/${encodeURIComponent(alias)}`);
  }

  async deleteNetwork(id: string): Promise<void> {
    await this.request("DELETE", `/api/micro-networks/${id}`);
  }

  async waitUntilDeletable(id: string, timeoutMs = 30_000): Promise<VmRow | null> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const vm = (await this.listVms()).find((row) => row.id === id);
      if (!vm) return null;
      if (vm.state === "created" || vm.state === "stopped" || vm.state === "error") {
        return vm;
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return (await this.listVms()).find((row) => row.id === id) ?? null;
  }

  /** Stop + delete every VM this suite owns (by name or imported template). */
  async deleteOwnedVms(alias: string, vmName = VM_NAME): Promise<void> {
    const vms = await this.listVms();
    const owned = vms.filter((vm) => vm.name === vmName || vm.template === alias);
    for (const vm of owned) {
      if (vm.state === "running" || vm.state === "starting" || vm.state === "stopping") {
        await this.stopVm(vm.id);
        await this.waitUntilDeletable(vm.id);
      }
      await this.deleteVm(vm.id);
    }
  }

  /** Remove the imported template if this run (or a previous one) registered it. */
  async deleteImportedImage(alias: string): Promise<void> {
    const images = await this.listImages();
    if (!images.some((image) => image.alias === alias && image.installed)) return;
    await this.deleteImage(alias);
  }

  async deleteOwnedNetwork(
    createdNetworkId: string | null,
    networkName = NETWORK_NAME,
  ): Promise<void> {
    if (!createdNetworkId) return;
    const networks = await this.listNetworks();
    const row = networks.find((network) => network.id === createdNetworkId);
    if (!row || row.name !== networkName) return;
    await this.deleteNetwork(createdNetworkId);
  }

  /** Delete every MicroNetwork with one of the given names (best-effort). */
  async deleteNetworksByName(names: string[]): Promise<void> {
    const wanted = new Set(names);
    const networks = await this.listNetworks();
    for (const row of networks) {
      if (!wanted.has(row.name)) continue;
      await this.deleteNetwork(row.id);
    }
  }
}
