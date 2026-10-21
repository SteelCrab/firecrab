import { createContext, useContext } from "react";
import type { ReactNode } from "react";
import { PANEL_WIDTH } from "../../lib/detailPanel";

/** App-wide state every screen frame shows: the banner, the VM poll note, the detail panel. */
export interface ScreenStatusValue {
  banner: ReactNode;
  pollNote: string;
  /** Whether the user keeps the wide-screen detail column open. */
  detailOpen: boolean;
  setDetailOpen: (open: boolean) => void;
  /** Width the user gave the detail panel, before this screen's room limits it. */
  detailWidth: number;
  setDetailWidth: (width: number) => void;
}

export const ScreenStatus = createContext<ScreenStatusValue>({
  banner: null,
  pollNote: "",
  detailOpen: true,
  setDetailOpen: () => {},
  detailWidth: PANEL_WIDTH.initial,
  setDetailWidth: () => {},
});

export function useScreenStatus(): ScreenStatusValue {
  return useContext(ScreenStatus);
}
