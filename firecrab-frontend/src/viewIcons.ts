import { Atom, Boxes, CalendarClock, Cpu, HardDrive, Layers, Network, Server, SquareTerminal, Workflow } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ViewId } from "./navigation";

/**
 * Line icon for every dashboard destination (redesign spec §5.4). Kept out of
 * component files so they keep exporting only components (Vite fast refresh).
 */
export const VIEW_ICONS: Record<ViewId, LucideIcon> = {
  vms: Cpu,
  services: Boxes,
  schedules: CalendarClock,
  networks: Network,
  pipe: Workflow,
  storages: HardDrive,
  images: Layers,
  kernels: Atom,
  shells: SquareTerminal,
  host: Server,
};
