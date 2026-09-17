import type { ViewId } from "../navigation";

/**
 * One icon per nav destination. `shells` keeps its own bash.png brand mark in
 * `Shell.tsx` and has no entry here; `kernels` uses the official Tux asset.
 */
export type NavIconId = Exclude<ViewId, "shells">;

export default function NavIcon({ id, className }: { id: NavIconId; className?: string }) {
  switch (id) {
    // Compute instance: a chip with four pins — MicroVMs are the workload
    // running on virtualized "hardware".
    case "vms":
      return (
        <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
          <rect x="7" y="7" width="10" height="10" rx="1.5" />
          <rect x="10.4" y="1.5" width="3.2" height="4" rx="1" />
          <rect x="10.4" y="18.5" width="3.2" height="4" rx="1" />
          <rect x="1.5" y="10.4" width="4" height="3.2" rx="1" />
          <rect x="18.5" y="10.4" width="4" height="3.2" rx="1" />
        </svg>
      );
    // Three linked nodes — subnets and bridges as a small mesh.
    case "networks":
      return (
        <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
          <line x1="12" y1="4.5" x2="4.5" y2="18" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
          <line x1="12" y1="4.5" x2="19.5" y2="18" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
          <line x1="4.5" y1="18" x2="19.5" y2="18" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
          <circle cx="12" cy="4.5" r="2.6" />
          <circle cx="4.5" cy="18" r="2.6" />
          <circle cx="19.5" cy="18" r="2.6" />
        </svg>
      );
    // Disk canister — M2Image/rootfs volumes.
    case "storages":
      return (
        <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
          <rect x="4" y="6" width="16" height="14" rx="2" />
          <ellipse cx="12" cy="6" rx="8" ry="2.6" />
        </svg>
      );
    // Picture frame with a sun and a mountain ridge — MicroImage catalog.
    case "images":
      return (
        <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
          <rect x="2.5" y="4" width="19" height="2" rx="1" />
          <rect x="2.5" y="18" width="19" height="2" rx="1" />
          <rect x="2.5" y="4" width="2" height="16" rx="1" />
          <rect x="19.5" y="4" width="2" height="16" rx="1" />
          <circle cx="8" cy="9.5" r="1.8" />
          <path d="M5 17l4.5-5.5L13 15l2.5-3L19 17z" />
        </svg>
      );
    // Official Tux mascot — independently managed Linux kernels.
    case "kernels":
      return (
        <img
          src="/tux.svg"
          alt=""
          className={className}
          width={18}
          height={18}
          aria-hidden="true"
        />
      );
    // Desktop + stand — this host machine, as distinct from the VMs it runs.
    case "host":
      return (
        <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
          <rect x="3" y="4" width="18" height="12" rx="1.5" />
          <rect x="9.5" y="17.5" width="5" height="2" rx="1" />
          <rect x="6.5" y="20" width="11" height="1.6" rx="0.8" />
        </svg>
      );
  }
}
