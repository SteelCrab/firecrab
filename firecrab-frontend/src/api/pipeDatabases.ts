import { fetchJson } from "./client";

export type DatabaseProvider = "local" | "supabase" | "rds";
export type DatabaseTlsMode = "disable" | "require" | "verify_full";
export interface DatabaseInput {
  name: string;
  provider: DatabaseProvider;
  uri: string;
  tlsMode?: DatabaseTlsMode;
  caCertificate?: string;
  writeEnabled?: boolean;
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
  writeEnabled?: boolean;
}
export interface ColumnInfo {
  name: string;
  dataType: string;
  nullable: boolean;
  defaultValue: string | null;
  generated: boolean;
  primaryKey: boolean;
  binary: boolean;
}
export interface EditableRow {
  values: Record<string, string | null>;
  key: Record<string, string | null>;
  version: string;
  truncatedColumns: string[];
}
export interface EditablePage {
  definition: {
    columns: ColumnInfo[];
    primaryKey: string[];
    transactional: boolean;
  };
  rows: EditableRow[];
  offset: number;
  hasMore: boolean;
}
export interface RowMutation {
  requestId: string;
  schema: string;
  table: string;
  operation: "insert" | "update" | "delete";
  values: Record<string, unknown>;
  key: Record<string, unknown>;
  version: string | null;
}
export interface DatabaseBinding {
  id: string;
  databaseId: string;
  requestId: string;
  serviceId: string;
  access: "read" | "write";
  envName: string;
  ready: boolean;
  tableCount: number;
}
export interface DatabaseTrigger {
  id: string;
  databaseId: string;
  requestId: string;
  schema: string;
  table: string;
  serviceId: string;
  path: string;
  paused: boolean;
  installed: boolean;
  detached: boolean;
  pending: number;
  failed: number;
  delivered: number;
  skipped: number;
  lastError: string | null;
}
export interface KafkaStatus {
  configured: boolean;
  brokers?: string[];
  tls?: boolean;
  authenticated?: boolean;
  development?: boolean;
}
export interface ManagedKafka {
  id: string;
  phase: string;
  development: boolean;
  error: string | null;
  brokers: { id: string; ipv4: string }[];
}
export interface KafkaInput {
  brokers: string[];
  tls: boolean;
  caCertificate?: string;
  username?: string;
  password?: string;
  development: boolean;
  clientCertificate?: string;
  clientKey?: string;
}
export interface DatabaseTable {
  schema: string;
  name: string;
}
export interface DatabasePreview {
  columns: { name: string; dataType: string }[];
  rows: (string | null)[][];
  hasMore: boolean;
  offset: number;
}
const base = "/api/pipe/databases";
function post<T>(url: string, body: unknown, method = "POST"): Promise<T> {
  return fetchJson(url, {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}
export function listPipeDatabases(
  signal?: AbortSignal,
): Promise<PipeDatabase[]> {
  return fetchJson(base, { signal });
}
export function testPipeDatabase(
  input: DatabaseInput,
): Promise<{ connected: boolean }> {
  return post(base + "/test", input);
}
export function createPipeDatabase(
  input: DatabaseInput,
): Promise<PipeDatabase> {
  return post(base, input);
}
export function deletePipeDatabase(id: string): Promise<{ deleted: boolean }> {
  return fetchJson(base + "/" + encodeURIComponent(id), { method: "DELETE" });
}
export function getDatabaseTables(
  id: string,
  signal?: AbortSignal,
): Promise<{ tables: DatabaseTable[]; truncated: boolean }> {
  return fetchJson(base + "/" + encodeURIComponent(id) + "/tables", { signal });
}
export function previewDatabase(
  id: string,
  table: DatabaseTable,
  offset = 0,
): Promise<DatabasePreview> {
  return post(base + "/" + encodeURIComponent(id) + "/preview", {
    schema: table.schema,
    table: table.name,
    limit: 50,
    offset,
  });
}
export const setDatabaseWrites = (
  id: string,
  writeEnabled: boolean,
): Promise<PipeDatabase> =>
  post(`${base}/${encodeURIComponent(id)}/access`, { writeEnabled }, "PUT");
export const editableRows = (
  id: string,
  table: DatabaseTable,
  offset = 0,
): Promise<EditablePage> =>
  post(`${base}/${encodeURIComponent(id)}/rows`, {
    schema: table.schema,
    table: table.name,
    limit: 50,
    offset,
  });
export const mutateRow = (
  id: string,
  body: RowMutation,
): Promise<{ requestId: string; affectedRows: number }> =>
  post(`${base}/${encodeURIComponent(id)}/mutations`, body);
export const databaseBindings = (id: string): Promise<DatabaseBinding[]> =>
  fetchJson(`${base}/${encodeURIComponent(id)}/bindings`);
export const bindDatabase = (
  id: string,
  body: {
    requestId: string;
    serviceId: string;
    access: "read" | "write";
    envName?: string;
  },
): Promise<DatabaseBinding> =>
  post(`${base}/${encodeURIComponent(id)}/bindings`, {
    requestId: body.requestId,
    serviceId: body.serviceId,
    access: body.access,
    ...(body.envName ? { envName: body.envName } : {}),
  });
export const removeBinding = (id: string): Promise<unknown> =>
  fetchJson(`/api/pipe/database-bindings/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
export const databaseTriggers = (id: string): Promise<DatabaseTrigger[]> =>
  fetchJson(`${base}/${encodeURIComponent(id)}/triggers`);
export const createDatabaseTrigger = (
  id: string,
  body: {
    requestId: string;
    schema: string;
    table: string;
    serviceId: string;
    path?: string;
  },
): Promise<DatabaseTrigger> =>
  post(`${base}/${encodeURIComponent(id)}/triggers`, {
    requestId: body.requestId,
    schema: body.schema,
    table: body.table,
    serviceId: body.serviceId,
    path: body.path,
  });
export const pauseDatabaseTrigger = (
  id: string,
  paused: boolean,
): Promise<unknown> =>
  post(
    `/api/pipe/database-triggers/${encodeURIComponent(id)}`,
    { paused },
    "PUT",
  );
export const detachDatabaseTrigger = (id: string): Promise<unknown> =>
  post(`/api/pipe/database-triggers/${encodeURIComponent(id)}/detach`, {});
export const removeDatabaseTrigger = (id: string): Promise<unknown> =>
  fetchJson(`/api/pipe/database-triggers/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
export const failedDatabaseEvents = (
  id: string,
): Promise<{ eventId: string; attempts: number; error?: string | null }[]> =>
  fetchJson(`/api/pipe/database-triggers/${encodeURIComponent(id)}/failures`);
export const replayDatabaseEvent = (
  id: string,
  eventId: string,
): Promise<unknown> =>
  post(
    `/api/pipe/database-triggers/${encodeURIComponent(id)}/replay/${encodeURIComponent(eventId.split(":").at(-1)!)}`,
    {},
  );
export const kafkaStatus = (): Promise<KafkaStatus> =>
  fetchJson("/api/pipe/database-kafka");
export const acknowledgeDatabaseEvent = (id: string, eventId: string): Promise<unknown> =>
  post(`/api/pipe/database-triggers/${encodeURIComponent(id)}/acknowledge/${encodeURIComponent(eventId.split(":").at(-1)!)}`, { confirmEventId: eventId });
export const saveKafka = (input: KafkaInput): Promise<unknown> =>
  post("/api/pipe/database-kafka", input, "PUT");
export const managedKafkaStatus = (): Promise<ManagedKafka | null> =>
  fetchJson("/api/pipe/database-kafka/managed");
export const createManagedKafka = (
  development: boolean,
): Promise<ManagedKafka> =>
  post("/api/pipe/database-kafka/managed", { development });

export const updatePipeDatabase = (
  id: string,
  input: DatabaseInput,
): Promise<PipeDatabase> =>
  post(`${base}/${encodeURIComponent(id)}`, input, "PUT");
export interface DatabaseJob {
  id: string;
  databaseId: string;
  kind: "backup" | "migration" | "restore";
  status: string;
  createdAt: number;
  backupId: string | null;
  safetyBackupId: string | null;
  version: string | null;
  checksum: string | null;
  bytes: number | null;
  error: string | null;
}
export interface DatabaseTools {
  postgres: boolean;
  mysql: boolean;
  pgDump: string | null;
  mysqlDump: string | null;
}
export const databaseTools = (): Promise<DatabaseTools> =>
  fetchJson("/api/pipe/database-maintenance/tools");
export const databaseJobs = (id: string): Promise<DatabaseJob[]> =>
  fetchJson(`${base}/${encodeURIComponent(id)}/jobs`);
export const createDatabaseBackup = (
  id: string,
  requestId: string,
): Promise<DatabaseJob> =>
  post(`${base}/${encodeURIComponent(id)}/backups`, { requestId });
export const migrateDatabase = (
  id: string,
  input: {
    requestId: string;
    version: string;
    sql: string;
    confirmDatabase: string;
    externalWritersStopped: boolean;
  },
): Promise<DatabaseJob> =>
  post(`${base}/${encodeURIComponent(id)}/migrations`, input);
export const restoreDatabase = (
  id: string,
  input: {
    requestId: string;
    backupId: string;
    confirmDatabase: string;
    externalWritersStopped: boolean;
  },
): Promise<DatabaseJob> =>
  post(`${base}/${encodeURIComponent(id)}/restore`, input);
export const removeDatabaseBackup = (id: string): Promise<unknown> =>
  fetchJson(`/api/pipe/database-jobs/${encodeURIComponent(id)}/backup`, {
    method: "DELETE",
  });
