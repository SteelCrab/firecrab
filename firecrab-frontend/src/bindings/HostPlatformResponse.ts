// Mirrors firecrab_api_types::HostPlatformResponse (camelCase wire shape).
import type { HostOs } from "./HostOs";

export type HostPlatformResponse = {
  os: HostOs;
  /** "macOS", "Windows 11", or the Linux distribution's pretty name. */
  name: string;
  version: string | null;
  architecture: string;
  /** How Firecrab's Linux runs on the host; null on a Linux host. */
  virtualization: string | null;
  /** The Linux system Firecrab itself runs on. */
  system: string;
  kernel: string;
};
