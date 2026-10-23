import { fetchJson } from "./client";

export interface AiConnection {
  id: string;
  name: string;
  provider: "openai" | "anthropic";
  authentication: "api_key" | "chatgpt" | "claude_code";
  endpoint: string;
  models: string[];
  streaming: boolean;
  toolCalls: boolean;
}
export interface AiConnectionInput { name: string; provider: "openai"; apiKey: string }
const base = "/api/pipe/ai";
const write = (method: string, body: unknown): RequestInit => ({ method, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
export const listAiConnections = (signal?: AbortSignal) => fetchJson<AiConnection[]>(`${base}/connections`, { signal });
export const saveAiConnection = (id: string, input: AiConnectionInput) => fetchJson<AiConnection>(`${base}/connections/${encodeURIComponent(id)}`, write("PUT", input));
export const disconnectAi = (id: string) => fetchJson<{ deleted: boolean; remoteRevocationConfirmed: boolean | null }>(`${base}/connections/${encodeURIComponent(id)}`, { method: "DELETE" });
export interface AiLogin { loginId: string; authorizationUrl: string; expiresIn: number }
export const startAiLogin = (id: string, name: string) => fetchJson<AiLogin>(`${base}/connections/${encodeURIComponent(id)}/login`, write("POST", { name }));
export const getAiLogin = (id: string, signal?: AbortSignal) => fetchJson<{ status: string; message: string | null }>(`${base}/logins/${encodeURIComponent(id)}`, { signal });
export type CliState = "connected" | "signed_out" | "unavailable" | "signing_in" | "error";
export interface CliLogin { connection: AiConnection; ticket: string; expiresIn: number }
export const startClaudeLogin = (id: string, name: string) => fetchJson<CliLogin>(`${base}/connections/${encodeURIComponent(id)}/claude-code/login`, write("POST", { name }));
export const getClaudeStatus = (id: string, signal?: AbortSignal) => fetchJson<{ status: CliState }>(`${base}/connections/${encodeURIComponent(id)}/claude-code/status`, { signal });
