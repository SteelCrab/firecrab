import type { VmResponse } from "./bindings";

export interface PipeApp {
  id: string;
  name: string;
  port: number | null;
  address: string | null;
  unavailable: "notRunning" | "noAddress" | "noPorts" | null;
}

export function pipeApps(vms: VmResponse[]): PipeApp[] {
  return vms.flatMap(vm => {
    const ports = [...new Set((vm.portForwards ?? [])
      .filter(rule => rule.protocol === "tcp" && Number.isInteger(rule.guestPort) && rule.guestPort > 0 && rule.guestPort <= 65535)
      .map(rule => rule.guestPort))].sort((a, b) => a - b);
    const unavailable = vm.state !== "running" ? "notRunning" : !vm.ipv4 ? "noAddress" : !ports.length ? "noPorts" : null;
    return (ports.length ? ports : [null]).map(port => ({
      id: `${vm.id}:${port ?? "none"}`,
      name: vm.name,
      port,
      address: vm.ipv4 && port !== null ? `${vm.ipv4}:${port}` : null,
      unavailable,
    }));
  });
}

export function canUsePipeApps(apps: PipeApp[], addresses: string[], preserved: string[] = []): boolean {
  return addresses.length > 0 && addresses.every(address => preserved.includes(address)
    || apps.some(app => app.address === address && app.unavailable === null));
}
