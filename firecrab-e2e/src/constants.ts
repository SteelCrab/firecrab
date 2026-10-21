import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Repository root (parent of this dedicated e2e package). */
export const REPO_ROOT = path.resolve(here, "../..");

/** Fixed loopback port used by `scripts/oci-e2e-registry.py` for this E2E. */
export const REGISTRY_PORT = Number.parseInt(
  process.env.FIRECRAB_OCI_E2E_PORT ?? "15555",
  10,
);

/** Deterministic reference testers type when the fixture binds 15555. */
export const FIXED_REFERENCE = `127.0.0.1:${REGISTRY_PORT}/firecrab/e2e:ready`;

/** Alias `POST /api/oci/import` claims for {@link FIXED_REFERENCE}. */
export const FIXED_ALIAS = `127.0.0.1-${REGISTRY_PORT}-firecrab-e2e-ready`;

/** Guest service line printed by the fixture image after boot. */
export const READY_SENTINEL = "FIRECRAB_OCI_E2E_READY";

/** Guest readiness line the API waits for before reporting `running`. */
export const NETWORK_READY = "FIRECRAB_NETWORK_READY";

/** VM created by the guest-boot half of the suite. */
export const VM_NAME = "oci-e2e-ready";

/** Dedicated MicroNetwork for the guest-boot half. */
export const NETWORK_NAME = "oci-e2e";

/** Isolated subnet so this suite does not share a developer's default net. */
export const NETWORK_CIDR = "172.30.90.0/24";

/** Loopback port for the #108 register spec's OCI fixture. Distinct from {@link REGISTRY_PORT}. */
export const REGISTER_REGISTRY_PORT = Number.parseInt(
  process.env.FIRECRAB_OCI_REGISTER_E2E_PORT ?? "15556",
  10,
);

/** Deterministic reference when the register fixture binds {@link REGISTER_REGISTRY_PORT}. */
export const REGISTER_FIXED_REFERENCE = `127.0.0.1:${REGISTER_REGISTRY_PORT}/firecrab/e2e:ready`;

/** Alias `POST /api/oci/import` claims for {@link REGISTER_FIXED_REFERENCE}. */
export const REGISTER_FIXED_ALIAS = `127.0.0.1-${REGISTER_REGISTRY_PORT}-firecrab-e2e-ready`;

/** Catalog version typed into the MicroRegistry register form. */
export const REGISTER_VERSION = "1";

/** VM created by the guest-boot half of the register suite. */
export const REGISTER_VM_NAME = "register-e2e-ready";

/** Dedicated MicroNetwork for the register guest-boot half. */
export const REGISTER_NETWORK_NAME = "register-e2e";

/** Isolated subnet so the register spec does not share {@link NETWORK_CIDR}. */
export const REGISTER_NETWORK_CIDR = "172.30.91.0/24";

/** IPv4-only MicroNetwork created by the #146 IPv6 dashboard spec. */
export const IPV6_E2E_V4_NAME = "ipv6-e2e-v4";

/** Isolated subnet for {@link IPV6_E2E_V4_NAME}. */
export const IPV6_E2E_V4_CIDR = "172.30.92.0/24";

/** Dual-stack MicroNetwork created by the #146 IPv6 dashboard spec (auto ULA). */
export const IPV6_E2E_V6_NAME = "ipv6-e2e-v6";

/** Isolated subnet for {@link IPV6_E2E_V6_NAME}. */
export const IPV6_E2E_V6_CIDR = "172.30.93.0/24";

/** Loopback port for the OCI DHCP-boot spec. Distinct from import/register. */
export const DHCP_REGISTRY_PORT = Number.parseInt(
  process.env.FIRECRAB_OCI_DHCP_E2E_PORT ?? "15557",
  10,
);

/** Deterministic reference when the DHCP-boot fixture binds {@link DHCP_REGISTRY_PORT}. */
export const DHCP_FIXED_REFERENCE = `127.0.0.1:${DHCP_REGISTRY_PORT}/firecrab/e2e:ready`;

/** Alias `POST /api/oci/import` claims for {@link DHCP_FIXED_REFERENCE}. */
export const DHCP_FIXED_ALIAS = `127.0.0.1-${DHCP_REGISTRY_PORT}-firecrab-e2e-ready`;

/** VM created by the OCI DHCP-boot spec (busybox `udhcpc`, same path as nginx-stable). */
export const DHCP_VM_NAME = "oci-e2e-dhcp";

/** Dedicated MicroNetwork for the OCI DHCP-boot spec. */
export const DHCP_NETWORK_NAME = "oci-e2e-dhcp";

/** Isolated subnet so this spec does not share {@link NETWORK_CIDR}. */
export const DHCP_NETWORK_CIDR = "172.30.94.0/24";

/** Host port forwarded to guest 80 — away from a developer's own 8888. */
export const DHCP_HOST_PORT = 18888;

/** Host port the SSH panel forwards to guest 22 — away from {@link DHCP_HOST_PORT}. */
export const DHCP_SSH_HOST_PORT = 18022;

/** Guest line when DHCP never handed an address. */
export const NETWORK_FAILED = "FIRECRAB_NETWORK_FAILED";

export const DEFAULT_API_URL = "http://127.0.0.1:5523";
export const DEFAULT_BASE_URL = "http://localhost:8080";

export function envFlag(name: string): boolean {
  const value = process.env[name];
  return value === "1" || value === "true" || value === "yes";
}

/**
 * Skip only the guest-boot half (create VM / start / console sentinels).
 * Inspect + import still run in the browser against the local fixture.
 */
export const SKIP_GUEST_BOOT = envFlag("FIRECRAB_E2E_SKIP_GUEST_BOOT");

export function apiUrl(): string {
  return (process.env.FIRECRAB_E2E_API_URL ?? DEFAULT_API_URL).replace(/\/$/, "");
}

export function dashboardUrl(): string {
  return (process.env.FIRECRAB_E2E_BASE_URL ?? DEFAULT_BASE_URL).replace(/\/$/, "");
}
