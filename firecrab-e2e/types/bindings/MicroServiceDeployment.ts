import type { MicroServiceSpec } from "./MicroServiceSpec";
import type { VmState } from "./VmState";

export type ServicePhase = "queued" | "importing" | "starting" | "checking_health" | "connecting_domain" | "switching_traffic" | "draining" | "rolling_back" | "running" | "degraded" | "stopping" | "stopped" | "failed";
export type DeployStrategy = "rolling" | "blue_green";
export type UpdateServiceRequest = { operationId: string; expectedReleaseId?: string; spec: MicroServiceSpec; strategy?: DeployStrategy };
export type RollbackServiceRequest = { operationId: string; expectedReleaseId?: string; releaseId: string; strategy?: DeployStrategy };
export type ServiceUpdate = {
  failure?: string | null; cancelled: boolean; previous: MicroServiceDeployment; requestedSpec: MicroServiceSpec;
  strategy: DeployStrategy; rollbackOf: string | null; restoring: boolean; routedNew: boolean;
  retiring: string[]; completed: boolean;
  transition: { expectedDeploymentId: string; expectedUpstreams: string[]; nextDeploymentId: string; nextUpstreams: string[]; workers: string[]; confirmed: boolean } | null;
};
export type DeployMicroServiceRequest = { microNetworkId?: string | null; hostPort?: number };
export type ServiceInstance = { vmId: string; ipv4: string; hostPort: number; state: VmState; healthy: boolean };
export type MicroServiceDeployment = {
  id: string; serviceId: string; phase: ServicePhase; spec: MicroServiceSpec;
  request: DeployMicroServiceRequest; digest: string | null; instances: ServiceInstance[];
  log: string[]; error: string | null; domainRouteOwned?: boolean;
  update?: ServiceUpdate; startedAt?: number; finishedAt?: number | null;
};
