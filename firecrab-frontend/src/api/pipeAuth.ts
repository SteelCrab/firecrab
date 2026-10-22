import { fetchJson } from "./client";

export type AuthProvider = "aws" | "github" | "firecrab";
export interface AuthConnection {
  provider: AuthProvider;
  account: string;
  registry: string | null;
  region: string | null;
  services: ("registry" | "firecrab-tunnel")[];
}
export type AuthLogin = { provider: "github" | "firecrab"; token: string } | {
  provider: "aws"; accessKeyId: string; secretAccessKey: string; sessionToken: string; region: string;
};
const base = "/api/pipe/auth";
export const getPipeAuth = (signal?: AbortSignal) => fetchJson<AuthConnection[]>(base, { signal });
export const connectPipeAuth = (login: AuthLogin) => fetchJson<AuthConnection>(`${base}/${login.provider}`, {
  method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(login),
});
export const disconnectPipeAuth = (provider: AuthProvider) => fetchJson<void>(`${base}/${provider}`, { method: "DELETE" });
