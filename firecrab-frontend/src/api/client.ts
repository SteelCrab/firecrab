import type {
  ApiError,
  AssignVmStorageRequest,
  CreateMicroNetworkRequest,
  CreateMicroStorageRequest,
  CreateVmRequest,
  ErrorResponse,
  HostStatusResponse,
  ImageInstallResponse,
  ImageResponse,
  MicroNetworkDetailResponse,
  MicroNetworkResponse,
  MicroStorageDetailResponse,
  MicroStorageResponse,
  NetworkInfoResponse,
  PackageAction,
  StorageDeviceResponse,
  StorageRootResponse,
  UpdateMicroNetworkRequest,
  UpdateVmResourcesRequest,
  VmLogResponse,
  VmResponse,
} from "../bindings";

/** API failures split into what the server said vs. not reaching it at all. */
export class ApiClientError extends Error {
  readonly status?: number;
  readonly apiError?: ApiError;

  private constructor(message: string, status?: number, apiError?: ApiError) {
    super(message);
    this.name = "ApiClientError";
    this.status = status;
    this.apiError = apiError;
  }

  static api(status: number, error: ApiError): ApiClientError {
    let text = error.message;
    for (const [field, detail] of Object.entries(error.fields ?? {})) {
      text += ` (${field}: ${detail})`;
    }
    return new ApiClientError(text, status, error);
  }

  static transport(detail: string): ApiClientError {
    return new ApiClientError(`API에 연결할 수 없습니다: ${detail}`);
  }

  /** Per-field validation detail from a 400 response, if any. */
  fieldError(name: string): string | undefined {
    return this.apiError?.fields?.[name];
  }
}

function transportDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function fail(response: Response): Promise<ApiClientError> {
  try {
    const body = (await response.json()) as ErrorResponse;
    return ApiClientError.api(response.status, body.error);
  } catch {
    // Vite returns empty 502/503 when firecrab-api is down or restarting.
    if (response.status === 502 || response.status === 503 || response.status === 504) {
      return ApiClientError.transport(
        `API 서버에 연결할 수 없습니다 (HTTP ${response.status}). firecrab-api가 실행 중인지 확인하세요.`,
      );
    }
    return ApiClientError.transport(`unexpected response (HTTP ${response.status})`);
  }
}

async function fetchJson<T>(input: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(input, init);
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
  return (await response.json()) as T;
}

export function listVms(): Promise<VmResponse[]> {
  return fetchJson("/api/vms");
}

export function getVm(id: string): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}`);
}

export function getVmLog(id: string): Promise<VmLogResponse> {
  return fetchJson(`/api/vms/${id}/log`);
}

export function createVm(request: CreateVmRequest): Promise<VmResponse> {
  return fetchJson("/api/vms", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export function updateVmResources(id: string, request: UpdateVmResourcesRequest): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export function startVm(id: string): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}/start`, { method: "POST" });
}

export function stopVm(id: string): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}/stop`, { method: "POST" });
}

/** Install, remove, or update packages on a running VM (`POST /api/vms/{id}/packages`). */
export function runPackageAction(
  id: string,
  action: PackageAction["action"],
  packages: string[] = [],
): Promise<VmResponse> {
  return fetchJson(`/api/vms/${encodeURIComponent(id)}/packages`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action, packages }),
  });
}

export function getNetworkInfo(): Promise<NetworkInfoResponse> {
  return fetchJson("/api/network");
}

export function getHostStatus(): Promise<HostStatusResponse> {
  return fetchJson("/api/host");
}

/** Template registry aliases available for create (`GET /api/images`). */
export function listImages(): Promise<ImageResponse[]> {
  return fetchJson("/api/images");
}

/** Start package download + verification (`POST /api/images/{alias}/package`). */
export function startImagePackage(alias: string): Promise<ImageInstallResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/package`, {
    method: "POST",
  });
}

/** Poll package download + verification (`GET /api/images/{alias}/package`). */
export function getImagePackage(alias: string): Promise<ImageInstallResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/package`);
}

/** Install a prepared local package (`POST /api/images/{alias}/install`). */
export function startImageInstall(alias: string): Promise<ImageInstallResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/install`, {
    method: "POST",
  });
}

/** Poll image installation status + log (`GET /api/images/{alias}/install`). */
export function getImageInstall(alias: string): Promise<ImageInstallResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/install`);
}

/** Unregister template and delete orphan artifact files (`DELETE /api/images/{alias}`). */
export async function deleteImage(alias: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/${encodeURIComponent(alias)}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}

export function listStorageRoots(): Promise<StorageRootResponse[]> {
  return fetchJson("/api/storage");
}

export function listStorageDevices(): Promise<StorageDeviceResponse[]> {
  return fetchJson("/api/storage/devices");
}

export function listMicroStorages(): Promise<MicroStorageResponse[]> {
  return fetchJson("/api/micro-storages");
}

export function getMicroStorage(id: string): Promise<MicroStorageDetailResponse> {
  return fetchJson(`/api/micro-storages/${id}`);
}

export function createMicroStorage(request: CreateMicroStorageRequest): Promise<MicroStorageResponse> {
  return fetchJson("/api/micro-storages", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export async function deleteMicroStorage(id: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/micro-storages/${id}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}

export function assignVmStorage(id: string, request: AssignVmStorageRequest): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}/storage`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export async function deleteVm(id: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/vms/${id}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}

export function listMicroNetworks(): Promise<MicroNetworkResponse[]> {
  return fetchJson("/api/micro-networks");
}

export function getMicroNetwork(id: string): Promise<MicroNetworkDetailResponse> {
  return fetchJson(`/api/micro-networks/${id}`);
}

export function createMicroNetwork(request: CreateMicroNetworkRequest): Promise<MicroNetworkResponse> {
  return fetchJson("/api/micro-networks", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Switches one network's internet access on or off — everything else about
 *  a MicroNetwork is fixed once created. */
export function updateMicroNetwork(
  id: string,
  request: UpdateMicroNetworkRequest,
): Promise<MicroNetworkResponse> {
  return fetchJson(`/api/micro-networks/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export async function deleteMicroNetwork(id: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/micro-networks/${id}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}
