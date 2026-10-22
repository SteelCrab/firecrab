import { fetchJson } from "./client";

export type SecretProvider = "firecrab" | "infisical" | "awsSecretsManager";
export type TransferDirection = "import" | "export";

export interface SecretSource {
  connectionId: string;
  connectionName: string;
  provider: SecretProvider;
  remoteKey: string;
  importedAt: number;
}

export interface SecretSummary {
  id: string;
  name: string;
  description: string;
  version: number;
  createdAt: number;
  updatedAt: number;
  source: SecretSource | null;
}

export interface ConnectionSettings {
  baseUrl: string;
  projectId: string;
  environment: string;
  secretPath: string;
  region: string;
  prefix: string;
}

export interface SecretConnection {
  id: string;
  name: string;
  provider: SecretProvider;
  settings: ConnectionSettings;
  version: number;
  updatedAt: number;
}

export interface TransferPreview {
  id: string;
  connectionId: string;
  direction: TransferDirection;
  expiresAt: number;
  changes: { name: string; change: "add" | "update" | "unchanged" }[];
}

export const providerLabel = (provider: SecretProvider) => ({
  firecrab: "Firecrab · PipeSecrets", infisical: "Infisical", awsSecretsManager: "AWS Secrets Manager",
})[provider];

function send<T>(path: string, method: string, body?: unknown): Promise<T> {
  return fetchJson(`/api/secrets${path}`, {
    method, cache: "no-store",
    ...(body === undefined ? {} : { headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }),
  });
}

export const listSecrets = () => send<SecretSummary[]>("", "GET");
export const listSecretConnections = () => send<SecretConnection[]>("/connections", "GET");
export const saveSecret = (secret: { name: string; description: string; value?: string; version?: number }, id?: string) =>
  send<SecretSummary>(id ? `/${encodeURIComponent(id)}` : "", id ? "PUT" : "POST", secret);
export const deleteSecret = (id: string) => send(`/${encodeURIComponent(id)}`, "DELETE");
export const revealSecret = (id: string) => send<{ value: string; version: number }>(`/${encodeURIComponent(id)}/reveal`, "POST");
export const saveSecretConnection = (connection: { name: string; provider: SecretProvider; settings: ConnectionSettings; credentials: Record<string, string>; version?: number }, id?: string) =>
  send<SecretConnection>(id ? `/connections/${encodeURIComponent(id)}` : "/connections", id ? "PUT" : "POST", connection);
export const deleteSecretConnection = (id: string) => send(`/connections/${encodeURIComponent(id)}`, "DELETE");
export const previewSecretTransfer = (id: string, direction: TransferDirection, secretIds: string[] = []) =>
  send<TransferPreview>(`/connections/${encodeURIComponent(id)}/preview`, "POST", { direction, secretIds });
export const applySecretTransfer = (id: string, names: string[]) =>
  send<{ applied: string[]; failed: { name: string; message: string }[] }>(`/plans/${encodeURIComponent(id)}/apply`, "POST", { names });
