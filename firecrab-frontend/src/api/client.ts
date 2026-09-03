import type {
  ApiError,
  AssignVmStorageRequest,
  BootstrapResponse,
  CreateMicroNetworkRequest,
  CreateMicroStorageRequest,
  CreateShellRequest,
  CreateShellRevisionRequest,
  CreateVmRequest,
  ErrorResponse,
  HostStatusResponse,
  ImageInstallResponse,
  ImageResponse,
  KernelInstallResponse,
  KernelResponse,
  OciImportRequest,
  OciInspectResponse,
  MicroRegistryRegisterRequest,
  MicroRegistryRegisterResponse,
  MicroRegistryResponse,
  MicroNetworkDetailResponse,
  MicroNetworkResponse,
  MicroStorageDetailResponse,
  MicroStorageResponse,
  NetworkInfoResponse,
  SshHostKeyResponse,
  ShellDetailResponse,
  ShellResponse,
  ShellRevisionResponse,
  StorageDeviceResponse,
  StorageRootResponse,
  UpdateCheckResponse,
  UpdateImageKernelRequest,
  UpdateMicroNetworkRequest,
  UpdateStartResponse,
  UpdateVmShellsRequest,
  UpdateVmPortForwardsRequest,
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
    return new ApiClientError(`Unable to connect to the API: ${detail}`);
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
        `Unable to reach the API server (HTTP ${response.status}). Check that firecrab-api is running.`,
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

export function getSshHostKey(id: string): Promise<SshHostKeyResponse> {
  return fetchJson(`/api/vms/${id}/ssh-host-key`);
}

/** Reads the operator private key as text, for copying it to the clipboard. */
export async function fetchSshKeyPem(id: string): Promise<string> {
  let response: Response;
  try {
    response = await fetch(`/api/vms/${id}/ssh-key`);
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
  return response.text();
}

/** Downloads the operator private key. Filename comes from Content-Disposition. */
export async function downloadSshKey(id: string, fallbackName: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/vms/${id}/ssh-key`);
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
  const blob = await response.blob();
  const header = response.headers.get("Content-Disposition") ?? "";
  const match = /filename="([^"]+)"/.exec(header);
  const filename = match?.[1] ?? `firecrab-${fallbackName}.pem`;
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
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

export function getNetworkInfo(): Promise<NetworkInfoResponse> {
  return fetchJson("/api/network");
}

export function getHostStatus(): Promise<HostStatusResponse> {
  return fetchJson("/api/host");
}

/** Newest release vs. this build (`GET /api/update`), cached 30 minutes by the API. */
export function getUpdateCheck(): Promise<UpdateCheckResponse> {
  return fetchJson("/api/update");
}

/** Launch the detached updater (`POST /api/update`); answers 202 immediately. */
export function startUpdate(): Promise<UpdateStartResponse> {
  return fetchJson("/api/update", { method: "POST" });
}

/** Template registry aliases available for create (`GET /api/images`). */
export function listImages(): Promise<ImageResponse[]> {
  return fetchJson("/api/images");
}

/** Full public detail for one M2Image (`GET /api/images/{alias}`). */
export function getImage(alias: string): Promise<ImageResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}`);
}

/** Host-architecture digest-pinned kernel catalog (`GET /api/kernels`). */
export function listKernels(): Promise<KernelResponse[]> {
  return fetchJson("/api/kernels");
}

/** Start a kernel package download + verification. */
export function startKernelInstall(version: string): Promise<KernelInstallResponse> {
  return fetchJson(`/api/kernels/${encodeURIComponent(version)}/install`, {
    method: "POST",
  });
}

/** Poll one kernel package job. */
export function getKernelInstall(version: string): Promise<KernelInstallResponse> {
  return fetchJson(`/api/kernels/${encodeURIComponent(version)}/install`);
}

/** Delete one unused installed kernel. */
export async function deleteKernel(version: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/kernels/${encodeURIComponent(version)}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) throw await fail(response);
}

/** Pair an installed image with an installed managed kernel. */
export function updateImageKernel(
  alias: string,
  request: UpdateImageKernelRequest,
): Promise<ImageResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/kernel`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Published M2Image packages and this host's matching cache/install state. */
export function getMicroRegistry(): Promise<MicroRegistryResponse> {
  return fetchJson("/api/microregistry");
}

/** Start a MicroRegistry register job (`POST /api/microregistry/register`). Returns 202 + snapshot. */
export function startMicroRegistryRegister(
  request: MicroRegistryRegisterRequest,
): Promise<MicroRegistryRegisterResponse> {
  return fetchJson("/api/microregistry/register", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Poll a MicroRegistry register job (`GET /api/microregistry/jobs/{jobId}`). */
export function getMicroRegistryRegisterJob(jobId: string): Promise<MicroRegistryRegisterResponse> {
  return fetchJson(`/api/microregistry/jobs/${encodeURIComponent(jobId)}`);
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

/** Resolve whether an OCI reference can run on this host (`GET /api/oci/inspect`). */
export function inspectOciImage(reference: string): Promise<OciInspectResponse> {
  return fetchJson(`/api/oci/inspect?${new URLSearchParams({ reference })}`);
}

/** Start an async OCI import (`POST /api/oci/import`). Returns 202 + ImageInstallResponse. */
export function startOciImport(request: OciImportRequest): Promise<ImageInstallResponse> {
  return fetchJson("/api/oci/import", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Poll an OCI import job (`GET /api/oci/import/{alias}`). */
export function getOciImport(alias: string): Promise<ImageInstallResponse> {
  return fetchJson(`/api/oci/import/${encodeURIComponent(alias)}`);
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

/** Delete a staged-but-not-installed package (`DELETE /api/images/{alias}/package`). */
export async function deleteStagedPackage(alias: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/${encodeURIComponent(alias)}/package`, { method: "DELETE" });
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

/** PATCH internet access and/or stored uplink. Omit `uplink` to leave it;
 *  send `""` to reset to the host default-route iface. */
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

export function listShells(): Promise<ShellResponse[]> {
  return fetchJson("/api/shells");
}

export function getShell(id: string): Promise<ShellDetailResponse> {
  return fetchJson(`/api/shells/${id}`);
}

export function createShell(request: CreateShellRequest): Promise<ShellRevisionResponse> {
  return fetchJson("/api/shells", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export function createShellRevision(
  id: string,
  request: CreateShellRevisionRequest,
): Promise<ShellRevisionResponse> {
  return fetchJson(`/api/shells/${id}/revisions`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Full body of one immutable revision (including past versions). */
export function getShellRevision(
  shellId: string,
  revisionId: string,
): Promise<ShellRevisionResponse> {
  return fetchJson(`/api/shells/${shellId}/revisions/${revisionId}`);
}

export async function deleteShell(id: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/shells/${id}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}

export function updateVmShells(id: string, request: UpdateVmShellsRequest): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}/shells`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

export function updateVmPortForwards(id: string, request: UpdateVmPortForwardsRequest): Promise<VmResponse> {
  return fetchJson(`/api/vms/${id}/port-forwards`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}

/** Bootstrap a distro from scratch inside a builder VM (`POST /api/images/{alias}/bootstrap`). */
export function startBootstrap(alias: string): Promise<BootstrapResponse> {
  return fetchJson(`/api/images/${encodeURIComponent(alias)}/bootstrap`, { method: "POST" });
}

/** Poll one bootstrap session (`GET /api/images/bootstrap/{bootstrapId}`). */
export function getBootstrap(bootstrapId: string): Promise<BootstrapResponse> {
  return fetchJson(`/api/images/bootstrap/${encodeURIComponent(bootstrapId)}`);
}

/**
 * The bootstrap still running on this host, if any
 * (`GET /api/images/bootstrap`).
 *
 * `startBootstrap` returns the session id, but only to the page that issued
 * it — reload that page, or leave and come back, and the id is gone while
 * the build keeps going. This asks the server for it instead, which is what
 * lets the session panel and its console reappear. Resolves to `null` when
 * nothing is building.
 */
export function getActiveBootstrap(): Promise<BootstrapResponse | null> {
  return fetchJson(`/api/images/bootstrap`);
}

/** Cancel a bootstrap and delete its builder VM (`DELETE /api/images/bootstrap/{bootstrapId}`). */
export async function cancelBootstrap(bootstrapId: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/bootstrap/${encodeURIComponent(bootstrapId)}`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}
