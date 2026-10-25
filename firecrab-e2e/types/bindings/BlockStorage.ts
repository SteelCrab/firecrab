/** Wire types from firecrab-api-types/src/block_storage.rs. */
export type MicroBlockStorageResponse = {
  id: string;
  name: string;
  sizeGib: number;
  storageRoot: string;
  state: "creating" | "available" | "attached" | "deleting";
  attachedVmId: string | null;
  mountPath: string | null;
};

export type CreateMicroBlockStorageRequest = {
  name: string;
  sizeGib: number;
  storageRoot?: string | null;
};

export type VmBlockStorageMount = {
  volumeId: string;
  mountPath: string;
};

export type AttachMicroBlockStorageRequest = {
  vmId: string;
  mountPath: string;
};
