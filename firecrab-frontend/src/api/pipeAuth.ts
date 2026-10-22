import { fetchJson } from "./client";

export type AuthProvider = "aws" | "github";
export interface AuthConnection {
  provider: AuthProvider;
  account: string;
  registry: string;
  region: string | null;
  services: "registry"[];
}
export type AuthLogin = { provider: "github"; token: string } | {
  provider: "aws"; accessKeyId: string; secretAccessKey: string; sessionToken: string; region: string;
};
const base = "/api/pipe/auth";
export const getPipeAuth = (signal?: AbortSignal) => fetchJson<AuthConnection[]>(base, { signal });
export const connectPipeAuth = (login: AuthLogin) => fetchJson<AuthConnection>(`${base}/${login.provider}`, {
  method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(login),
});
export const disconnectPipeAuth = (provider: AuthProvider) => fetchJson<void>(`${base}/${provider}`, { method: "DELETE" });
