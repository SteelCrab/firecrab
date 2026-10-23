import type { FirecrabInfo } from "./FirecrabInfo";

// Mirrors firecrab_api_types::FirecrabInfoResponse. Runtime fields are optional
// here so a newer dashboard can still display an older API's install info.
export type FirecrabInfoResponse = FirecrabInfo & {
  firecrackerVersion?: string | null;
  apiService?: string;
  netHelperService?: string;
  helperUnit?: string;
};
