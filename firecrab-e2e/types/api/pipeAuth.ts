// API fixtures for browser tests. No application runtime code.
export type AuthProvider = "aws" | "github" | "firecrab";

export interface AuthConnection {
  provider: AuthProvider;
  account: string;
  accountId?: string;
  expiresAt?: number;
  registry: string | null;
  region: string | null;
  services: ("registry" | "storage")[];
}

export type AuthLogin = { provider: "github" | "firecrab"; token: string } | {
  provider: "aws"; accessKeyId: string; secretAccessKey: string; sessionToken: string; region: string;
};
