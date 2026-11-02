import { ApiClientError, fetchJson } from "./client";

type Pending = { operationId: string; endpointId: string };
type Operation = {
  operationId: string;
  action: string;
  state: "running" | "needs_recovery" | "succeeded" | "failed";
  ownership: { endpointId: string; resource: { kind: string; id: string } };
};
export type NativeMutationResult<T> = {
  value: T | null;
  recovered: boolean;
  acknowledge: () => void;
};
export class NativeMutationPending extends Error {}

/** Retain identities only. Credentials and request bodies never enter browser storage. */
export async function nativeMutation<T>(
  method: string,
  url: string,
  body: unknown,
  action: string,
  resourceKind: string,
  resolve: (id: string) => Promise<T | null>,
  resourceId?: string,
): Promise<NativeMutationResult<T>> {
  const key = `firecrab.native-mutation:${method}:${url}`;
  function parse(stored: string): Pending {
    let value: Pending;
    try {
      value = JSON.parse(stored) as Pending;
    } catch {
      throw new NativeMutationPending(
        "Original operation identity is unavailable; review Stack operations.",
      );
    }
    if (
      !value ||
      Object.keys(value).length !== 2 ||
      typeof value.operationId !== "string" ||
      !value.operationId ||
      typeof value.endpointId !== "string" ||
      !value.endpointId
    )
      throw new NativeMutationPending(
        "Original operation identity is unavailable; review Stack operations.",
      );
    return { operationId: value.operationId, endpointId: value.endpointId };
  }
  const stored = sessionStorage.getItem(key);
  let recovered = stored !== null;
  let pending: Pending;
  if (stored !== null) pending = parse(stored);
  else {
    const endpoint = await fetchJson<{ endpointId: string }>(
      "/api/iac/identity",
    );
    if (typeof endpoint.endpointId !== "string" || !endpoint.endpointId)
      throw new Error("Cannot verify the database endpoint.");
    // A concurrent caller may have reserved the slot while identity was loading.
    const reserved = sessionStorage.getItem(key);
    if (reserved !== null) {
      pending = parse(reserved);
      recovered = true;
    } else {
      pending = {
        operationId: crypto.randomUUID(),
        endpointId: endpoint.endpointId,
      };
      sessionStorage.setItem(key, JSON.stringify(pending));
    }
  }
  const acknowledge = () => {
    // A late completion must not clear a newer request's identity.
    if (sessionStorage.getItem(key) === JSON.stringify(pending))
      sessionStorage.removeItem(key);
  };
  try {
    const value = await fetchJson<T>(url, {
      method,
      headers: {
        "Content-Type": "application/json",
        "x-firecrab-operation-id": pending.operationId,
        "x-firecrab-endpoint-id": pending.endpointId,
      },
      ...(body === undefined
        ? {}
        : {
            body: JSON.stringify(
              typeof body === "function" ? body(pending.operationId) : body,
            ),
          }),
    });
    // The mounted UI acknowledges only after it can display the saved resource.
    return { value, recovered, acknowledge };
  } catch (failure) {
    let operation: Operation;
    try {
      operation = await fetchJson<Operation>(
        `/api/iac/native-operations/${encodeURIComponent(pending.operationId)}`,
      );
    } catch {
      // A missing/unreachable receipt does not prove that the original request stopped.
      if (
        !(failure instanceof ApiClientError) ||
        failure.status === undefined ||
        failure.status >= 500
      )
        throw new NativeMutationPending(
          `${failure instanceof Error ? failure.message : "The database request failed."} Review the original request in Stack operations.`,
        );
      throw failure;
    }
    if (
      !operation ||
      operation.operationId !== pending.operationId ||
      operation.action !== action ||
      operation.ownership?.endpointId !== pending.endpointId ||
      operation.ownership?.resource?.kind !== resourceKind ||
      (resourceId !== undefined &&
        operation.ownership.resource.id !== resourceId)
    )
      throw new Error(
        "Original operation identity changed; review Stack operations.",
      );
    if (operation.state === "failed") {
      acknowledge();
      throw failure;
    }
    if (operation.state === "succeeded") {
      const value = await resolve(operation.ownership.resource.id);
      return { value, recovered: true, acknowledge };
    }
    if (operation.state === "running" || operation.state === "needs_recovery")
      throw new NativeMutationPending(
        operation.state === "running"
          ? "The original database request is still running. Review it in Stack operations."
          : "The original database request needs recovery. Review it in Stack operations.",
      );
    throw new Error(
      "Original operation state is unavailable; review Stack operations.",
    );
  }
}
