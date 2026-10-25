
export interface StorageSpec {
  name: string;
  bucket: string;
  region: string;
  prefix: string;
  credentialRef: "pipeAuthAws" | "connection";
}
export interface StorageConnection extends StorageSpec {
  id: string;
  services: { id: string; name: string }[];
}
export interface StorageInput extends StorageSpec {
  credentials?: { accessKeyId: string; secretAccessKey: string; sessionToken: string };
}
export interface ObjectPage {
  prefix: string;
  objects: { key: string; size: number; lastModified: string }[];
  prefixes: string[];
  nextCursor: string | null;
}
