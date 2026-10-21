import { createContext, useContext } from "react";
import type { ReactNode } from "react";

/** App-wide state every screen frame shows: the banner, the VM poll note, the detail panel. */
export interface ScreenStatusValue {
  banner: ReactNode;
  pollNote: string;
  /** Whether the user keeps the wide-screen detail column open. */
  detailOpen: boolean;
  setDetailOpen: (open: boolean) => void;
}

export const ScreenStatus = createContext<ScreenStatusValue>({
  banner: null,
  pollNote: "",
  detailOpen: true,
  setDetailOpen: () => {},
});

export function useScreenStatus(): ScreenStatusValue {
  return useContext(ScreenStatus);
}
