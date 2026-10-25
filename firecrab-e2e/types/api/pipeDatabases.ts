// API fixtures for browser tests. No application runtime code.
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
