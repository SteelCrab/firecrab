import type { ApiError, CreateVmRequest, ErrorResponse, VmResponse } from "../bindings";

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

export function createVm(request: CreateVmRequest): Promise<VmResponse> {
  return fetchJson("/api/vms", {
    method: "POST",
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
