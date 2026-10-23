import { fetchJson, fetchApiResponse } from "./client";

export interface StorageSpec {
  name: string;
  bucket: string;
  region: string;
  prefix: string;
  credentialRef: "pipeAuthAws" | "connection";
}
export interface StorageConnection extends StorageSpec {
  id: string;
  services: { id: string; name: string }[];
}
export interface StorageInput extends StorageSpec {
  credentials?: { accessKeyId: string; secretAccessKey: string; sessionToken: string };
}
export interface ObjectPage {
  prefix: string;
  objects: { key: string; size: number; lastModified: string }[];
  prefixes: string[];
  nextCursor: string | null;
}
const base = "/api/pipe/storage";
const path = (id: string) => `${base}/${encodeURIComponent(id)}`;
const objectPath = (id: string, key: string, overwrite = false) => `${path(id)}/object?${new URLSearchParams({ key, overwrite: String(overwrite) })}`;
export const listStorage = (signal?: AbortSignal) => fetchJson<StorageConnection[]>(base, { signal });
export const saveStorage = (input: StorageInput, id?: string) => fetchJson<StorageConnection>(id ? path(id) : base, {
  method: id ? "PUT" : "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(input),
});
export const disconnectStorage = (id: string) => fetchApiResponse(path(id), { method: "DELETE" });
export const verifyStorage = (id: string) => fetchJson<Record<"list" | "read" | "write" | "delete", "allowed" | "unknown">>(`${path(id)}/verify`, { method: "POST" });
export function listObjects(id: string, prefix: string, cursor?: string) {
  const query = new URLSearchParams({ prefix, limit: "100" });
  if (cursor) query.set("cursor", cursor);
  return fetchJson<ObjectPage>(`${path(id)}/objects?${query}`);
}
export const uploadObject = (id: string, key: string, file: File, overwrite: boolean) => fetchApiResponse(objectPath(id, key, overwrite), { method: "PUT", body: file, headers: { "Content-Type": "application/octet-stream" } });
export const removeObject = (id: string, key: string) => fetchApiResponse(objectPath(id, key), { method: "DELETE" });
export async function downloadObject(id: string, key: string) {
  const response = await fetchApiResponse(objectPath(id, key));
  const blob = await response.blob();
  const length = Number(response.headers.get("Content-Length"));
  if (!Number.isSafeInteger(length) || blob.size !== length) throw new Error("S3 download interrupted. Retry the download.");
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = key.split("/").at(-1) || "download";
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}
