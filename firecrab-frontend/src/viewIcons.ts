import { Bot, Shield, createLucideIcon, type LucideIcon } from "lucide-react";
import type { ViewId } from "./navigation";

/** Keep the established navigation symbols during the React migration. */
export const VIEW_ICONS: Record<ViewId, LucideIcon> = {
  vms: createLucideIcon("dashboard-vms", [
    [
      "path",
      {
        d: "M5 3h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm0 10h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2ZM7 7h.01M7 17h.01M11 7h6M11 17h6",
        key: "path",
      },
    ],
  ]),
  pools: createLucideIcon("dashboard-pools", [
    [
      "path",
      {
        d: "m12 3 10 5-10 5L2 8l10-5ZM2 12l10 5 10-5M2 16l10 5 10-5",
        key: "path",
      },
    ],
  ]),
  schedules: createLucideIcon("dashboard-schedules", [
    [
      "path",
      {
        d: "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v13a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM12 13v3l2 1",
        key: "path",
      },
    ],
  ]),
  services: createLucideIcon("dashboard-services", [
    [
      "path",
      {
        d: "m12 3 9 5v8l-9 5-9-5V8l9-5Zm0 10 9-5M12 13 3 8M12 13v8M7.5 5.5l9 5v5",
        key: "path",
      },
    ],
  ]),
  stacks: createLucideIcon("dashboard-stacks", [
    [
      "path",
      { d: "m12 3 9 5-9 5-9-5 9-5ZM3 12l9 5 9-5M3 16l9 5 9-5", key: "path" },
    ],
  ]),
  cluster: createLucideIcon("dashboard-cluster", [
    [
      "path",
      { d: "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z", key: "path" },
    ],
  ]),
  networks: createLucideIcon("dashboard-networks", [
    [
      "path",
      {
        d: "M9 3h6v6H9zM3 15h6v6H3zM15 15h6v6h-6zM12 9v3M6 15v-3h12v3",
        key: "path",
      },
    ],
  ]),
  "security-groups": createLucideIcon("dashboard-security-groups", [
    [
      "path",
      {
        d: "M12 3c3 2 6 3 8 3v6c0 5-5 8-8 10-3-2-8-5-8-10V6c2 0 5-1 8-3Zm-4 9 3 3 5-6",
        key: "path",
      },
    ],
  ]),
  pipe: createLucideIcon("dashboard-pipe", [
    ["path", { d: "M3 7h18m-4-4 4 4-4 4M21 17H3m4-4-4 4 4 4", key: "path" }],
  ]),
  queues: createLucideIcon("dashboard-queues", [
    [
      "path",
      { d: "M4 5h16M4 10h12M4 15h8M4 20h8M15 17h6m-3-3 3 3-3 3", key: "path" },
    ],
  ]),
  secrets: createLucideIcon("dashboard-secrets", [
    [
      "path",
      {
        d: "M21 8a5 5 0 1 1-10 0 5 5 0 0 1 10 0ZM12.5 11.5 3 21H2v-3l3-3h3v-3l4.5-.5M16 7h.01",
        key: "path",
      },
    ],
  ]),
  storages: createLucideIcon("dashboard-storages", [
    [
      "path",
      {
        d: "M21 5c0 2.2-4 4-9 4S3 7.2 3 5s4-4 9-4 9 1.8 9 4ZM3 5v14c0 2.2 4 4 9 4s9-1.8 9-4V5M3 12c0 2.2 4 4 9 4s9-1.8 9-4",
        key: "path",
      },
    ],
  ]),
  volumes: createLucideIcon("dashboard-volumes", [
    [
      "path",
      {
        d: "M22 12H2M5.5 5.1 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.5-6.9A2 2 0 0 0 16.8 4H7.2a2 2 0 0 0-1.7 1.1ZM6 16h.01M10 16h.01",
        key: "path",
      },
    ],
  ]),
  images: createLucideIcon("dashboard-images", [
    [
      "path",
      {
        d: "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2ZM7.5 7.5h.01M3 17l6-6 4 4 3-3 5 5",
        key: "path",
      },
    ],
  ]),
  kernels: createLucideIcon("dashboard-kernels", [
    [
      "path",
      {
        d: "M6 6h12v12H6zM9 9h6v6H9zM9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4",
        key: "path",
      },
    ],
  ]),
  shells: createLucideIcon("dashboard-shells", [
    ["path", { d: "m4 5 7 7-7 7M13 19h7", key: "path" }],
  ]),
  host: createLucideIcon("dashboard-host", [
    [
      "path",
      {
        d: "M4 3h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2ZM12 17v4M8 21h8",
        key: "path",
      },
    ],
  ]),
  "ai-sandbox": Bot,
  security: Shield,
};
