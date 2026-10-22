import { createContext, useContext } from "react";
import type { HostStatusResponse } from "./bindings";

/** Latest host status from `HostStatusProvider`; `null` before the first answer and after a failure. */
export const HostStatusContext = createContext<HostStatusResponse | null>(null);

export function useHostStatus(): HostStatusResponse | null {
  return useContext(HostStatusContext);
}
