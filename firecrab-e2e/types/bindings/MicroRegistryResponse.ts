// Mirrors firecrab_api_types::MicroRegistryResponse (camelCase wire shape).

import type { MicroRegistryImageResponse } from "./MicroRegistryImageResponse";

export type MicroRegistryResponse = {
  source: string;
  images: MicroRegistryImageResponse[];
};
