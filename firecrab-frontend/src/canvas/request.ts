// A restored generation must be acknowledged by reopening the page. Polling never adopts it.
const generations = new Map<string, number>();
const projectPath = /^\/api\/ai-sandbox\/projects\/([^/]+)(?:[/?]|$)/;
export class WorkspaceError extends Error {
  status?: number;
  code?: string;
  constructor(message: string, status?: number, code?: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
export const pause = (ms: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, ms));
export async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  retryBusy = true,
): Promise<T> {
  const project = projectPath.exec(path)?.[1];
  for (let attempt = 0; attempt < 8; attempt++) {
    let response: Response;
    try {
      response = await fetch(path, {
        method,
        credentials: "same-origin",
        headers: {
          Accept: "application/json",
          ...(project && generations.has(project) && method !== "GET"
            ? {
                "X-Firecrab-Workspace-Generation": String(
                  generations.get(project),
                ),
              }
            : {}),
          ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
    } catch (error) {
      throw new WorkspaceError(
        error instanceof Error ? error.message : "Workspace transport failed",
      );
    }
    if (response.status === 204) return undefined as T;
    const value = await response.json().catch(() => null);
    if (response.ok) {
      if (value === null)
        throw new WorkspaceError("Invalid workspace response");
      if (
        project &&
        method === "GET" &&
        path === `/api/ai-sandbox/projects/${project}/workspace` &&
        Number.isSafeInteger(value.generation)
      ) {
        if (!generations.has(project) || !value.writeGenerationRequired)
          generations.set(project, value.generation);
      }
      return value as T;
    }
    const code =
      typeof value?.error?.code === "string" ? value.error.code : undefined;
    const message =
      typeof value?.error?.message === "string"
        ? value.error.message
        : "Workspace request failed (" + response.status + ")";
    if (retryBusy && code === "workspace_busy" && attempt < 7) {
      await pause(100 * (attempt + 1));
      continue;
    }
    throw new WorkspaceError(message, response.status, code);
  }
  throw new WorkspaceError("Workspace is busy");
}
export const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
