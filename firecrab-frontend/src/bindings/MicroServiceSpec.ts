/** Desired MicroService configuration shared with firecrab-api-types. */
export type MicroServiceSpec = {
  name: string;
  image: string;
  runtime: "vm";
  port: number;
  healthPath: string;
  domain: string | null;
  ram: number;
  cpu: number;
  replicas: number;
  startupTimeout: number;
  drainTimeout: number;
  env: Record<string, string>;
  secrets: Record<string, string>;
};
