import { useEffect, useState } from "react";
import type { MicroServiceDeployment, MicroServiceResponse } from "../bindings";
import { getServiceDeployment, listMicroServices } from "../api/client";

export interface ServiceConnection {
  service: MicroServiceResponse;
  deployment: MicroServiceDeployment | null;
  error: string | null;
}

/** One shared snapshot for all groups; polling never replaces a rule draft. */
export function useSecurityGroupConnections(refreshVersion: number) {
  const [snapshot, setSnapshot] = useState<{
    services: ServiceConnection[]; loaded: boolean; error: string | null;
  }>({ services: [], loaded: false, error: null });

  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const services = await listMicroServices();
        const connections = await Promise.all(services.map(async (service): Promise<ServiceConnection> => {
          try { return { service, deployment: await getServiceDeployment(service.id), error: null }; }
          catch (failure) { return { service, deployment: null, error: (failure as Error).message }; }
        }));
        if (active) setSnapshot({ services: connections, loaded: true, error: null });
      } catch (failure) {
        if (active) setSnapshot({ services: [], loaded: true, error: (failure as Error).message });
      }
      if (active) timer = setTimeout(refresh, 10_000);
    };
    void refresh();
    return () => { active = false; clearTimeout(timer); };
  }, [refreshVersion]);

  return snapshot;
}
