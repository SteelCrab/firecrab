// API fixtures for browser tests. No application runtime code.
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
