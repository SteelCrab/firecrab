import type { MicroServiceSpec } from "./MicroServiceSpec";
import type { VmState } from "./VmState";

export type ServicePhase = "queued" | "importing" | "starting" | "checking_health" | "connecting_domain" | "running" | "degraded" | "stopping" | "stopped" | "failed";
export type DeployMicroServiceRequest = { microNetworkId?: string | null; hostPort?: number };
export type ServiceInstance = { vmId: string; ipv4: string; hostPort: number; state: VmState; healthy: boolean };
export type MicroServiceDeployment = {
  id: string; serviceId: string; phase: ServicePhase; spec: MicroServiceSpec;
  request: DeployMicroServiceRequest; digest: string | null; instances: ServiceInstance[];
  log: string[]; error: string | null; domainRouteOwned?: boolean; internalRouteOwned?: boolean;
};
