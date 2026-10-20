import { fetchJson } from "./client";
import type { ImageInstallResponse } from "../bindings";

export interface RegistryConnection { registry: string; username: string; provider: "dockerHub" | "ghcr" | "ecr" }
export interface RegistryImage { id: string; reference: string; digest: string; architecture: string; sizeBytes: number }
export interface RegistryTags { tags: string[]; nextLast: string | null }
export type RegistryTransfer = { action: "pull"; reference: string };
const base = "/api/pipe";
export const getRegistries = () => fetchJson<RegistryConnection[]>(`${base}/registries`);
export const saveRegistry = (registry: string, username: string, secret: string) => fetchJson<RegistryConnection>(`${base}/registries/${encodeURIComponent(registry)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username, secret }) });
export const deleteRegistry = (registry: string) => fetchJson<void>(`${base}/registries/${encodeURIComponent(registry)}`, { method: "DELETE" });
export const getRegistryImages = () => fetchJson<RegistryImage[]>(`${base}/registry-images`);
export const getRegistryTags = (reference: string, last?: string) => fetchJson<RegistryTags>(`${base}/registry-tags?${new URLSearchParams({ reference, ...(last ? { last } : {}) })}`);
export const startRegistryTransfer = (body: RegistryTransfer) => fetchJson<ImageInstallResponse>(`${base}/registry-transfers`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
export const getRegistryTransfer = (id: string) => fetchJson<ImageInstallResponse>(`${base}/registry-transfers/${encodeURIComponent(id)}`);
