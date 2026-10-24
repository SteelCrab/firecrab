import type { HostOs } from "./HostOs";

export type ManagerNetworkMode = "nat" | "bridged" | "mirrored";

export type ManagerResources = {
  cpu: number;
  memoryMib: number;
  diskGib: number;
  networkMode: ManagerNetworkMode;
  networkInterface: string | null;
};

export type ManagerResourceOperation = {
  id: string;
  state: "applying" | "complete" | "failed";
  error: string | null;
};

export type ManagerResourceStatus = {
  controllerId: string;
  os: HostOs;
  settings: ManagerResources;
  current: ManagerResources | null;
  host: {
    cpu: number;
    memoryMib: number;
    diskAvailableGib: number;
    networkInterfaces: string[];
    networkModes: ManagerNetworkMode[];
  };
  pending: boolean;
  sharedWsl: boolean;
  operation: ManagerResourceOperation | null;
};
