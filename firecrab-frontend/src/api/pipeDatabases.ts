import { fetchJson } from "./client";

export type DatabaseProvider = "local" | "supabase" | "rds";
export type DatabaseTlsMode = "disable" | "require" | "verify_full";
export interface DatabaseInput {
  name: string;
  provider: DatabaseProvider;
  uri: string;
  tlsMode?: DatabaseTlsMode;
  caCertificate?: string;
}
export interface PipeDatabase {
  id: string;
  name: string;
  provider: DatabaseProvider;
  engine: "postgres" | "mysql";
  host: string;
  port: number;
  database: string;
  tlsMode: DatabaseTlsMode;
}
export interface DatabaseTable { schema: string; name: string }
export interface DatabasePreview {
  columns: { name: string; dataType: string }[];
  rows: (string | null)[][];
  hasMore: boolean;
  offset: number;
}
const base = "/api/pipe/databases";
function post<T>(url: string, body: unknown): Promise<T> {
  return fetchJson(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
}
export function listPipeDatabases(signal?: AbortSignal): Promise<PipeDatabase[]> {
  return fetchJson(base, { signal });
}
export function testPipeDatabase(input: DatabaseInput): Promise<{ connected: boolean }> {
  return post(base + "/test", input);
}
export function createPipeDatabase(input: DatabaseInput): Promise<PipeDatabase> {
  return post(base, input);
}
export function deletePipeDatabase(id: string): Promise<{ deleted: boolean }> {
  return fetchJson(base + "/" + encodeURIComponent(id), { method: "DELETE" });
}
export function getDatabaseTables(id: string): Promise<{ tables: DatabaseTable[]; truncated: boolean }> {
  return fetchJson(base + "/" + encodeURIComponent(id) + "/tables");
}
export function previewDatabase(id: string, table: DatabaseTable, offset = 0): Promise<DatabasePreview> {
  return post(base + "/" + encodeURIComponent(id) + "/preview", { schema: table.schema, table: table.name, limit: 50, offset });
}
