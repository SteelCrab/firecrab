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
export interface RabbitMQStatus {
  configured: boolean;
  brokers?: string[];
  tls?: boolean;
  authenticated?: boolean;
  development?: boolean;
}
export interface ManagedRabbitMQ {
  id: string;
  phase: string;
  development: boolean;
  error: string | null;
  brokers: { id: string; ipv4: string }[];
}
export interface RabbitMQInput {
  brokers: string[];
  tls: boolean;
  caCertificate?: string;
  username: string;
  password: string;
  virtualHost?: string;
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
