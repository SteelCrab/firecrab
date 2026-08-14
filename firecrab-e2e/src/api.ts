import { apiUrl, NETWORK_NAME, VM_NAME } from "./constants.js";

interface VmRow {
  id: string;
  name: string;
  state: string;
  template: string;
}

interface ImageRow {
  alias: string;
  installed: boolean;
}

interface NetworkRow {
  id: string;
  name: string;
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
  async deleteOwnedVms(alias: string): Promise<void> {
    const vms = await this.listVms();
    const owned = vms.filter((vm) => vm.name === VM_NAME || vm.template === alias);
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

  async deleteOwnedNetwork(createdNetworkId: string | null): Promise<void> {
    if (!createdNetworkId) return;
    const networks = await this.listNetworks();
    const row = networks.find((network) => network.id === createdNetworkId);
    if (!row || row.name !== NETWORK_NAME) return;
    await this.deleteNetwork(createdNetworkId);
  }
}
