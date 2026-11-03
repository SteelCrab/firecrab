export type BackupPolicy = {
  enabled: boolean;
  intervalSeconds: number;
  nextRunAt: number;
  lastCheckpoint?: string;
  error?: string;
};
export type BackupOperation = {
  id: string;
  phase: string;
  pendingReconciliation?: boolean;
  createdAt: number;
  error?: string;
  restoredGeneration?: number;
  protected?: boolean;
  cleanupId?: string;
  cleanedAt?: number;
  request: { expectedGeneration: number; restoreCheckpoint?: string | null };
  manifest?: {
    checkpoint: { id: string; createdAt: number; revision: number };
    volumeBytes: number;
  };
};
export type BackupIntent = {
  id: string;
  expectedGeneration: number;
  restoreCheckpoint: string | null;
};
const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
export const backupKey = (project: string) =>
  `firecrab.backup-intent.${project}`;
export function readBackupIntent(
  storage: Pick<Storage, "getItem">,
  project: string,
): BackupIntent | undefined {
  const raw = storage.getItem(backupKey(project));
  if (!raw) return;
  const value = JSON.parse(raw) as BackupIntent;
  if (
    !value ||
    !uuid.test(value.id) ||
    !Number.isSafeInteger(value.expectedGeneration) ||
    value.expectedGeneration < 1 ||
    (value.restoreCheckpoint !== null && !uuid.test(value.restoreCheckpoint))
  )
    throw new Error(
      "Invalid retained backup request. Inspect its server operation before clearing it.",
    );
  return value;
}

export type BackupRetention = {
  enabled: boolean;
  keepLatest: number;
  maxAgeDays: number | null;
  includeManual: boolean;
  nextRunAt: number;
  lastCleanupAt?: number;
  error?: string;
};
export type CleanupCandidate = {
  id: string;
  kind: "checkpoint" | "partial" | "restore_originals";
  createdAt: number;
  eligible: boolean;
  protected: boolean;
  reason?: string;
};
export type CleanupPreview = {
  candidates: CleanupCandidate[];
  automaticTargetIds: string[];
};
export type CleanupIntent = {
  id: string;
  expectedGeneration: number;
  targetIds: string[];
};
export type CleanupOperation = {
  id: string;
  request: Omit<CleanupIntent, "id">;
  phase: string;
  pendingReconciliation: boolean;
  automatic: boolean;
  createdAt: number;
  logicalBytes: number;
  removedFiles: number;
  error?: string;
};
export const cleanupKey = (project: string) =>
  `firecrab.backup-cleanup.${project}`;
export function readCleanupIntent(
  storage: Pick<Storage, "getItem">,
  project: string,
): CleanupIntent | undefined {
  const raw = storage.getItem(cleanupKey(project));
  if (!raw) return;
  const v = JSON.parse(raw) as CleanupIntent;
  const nonzero = (s: string) =>
    uuid.test(s) && !/^0{8}-0{4}-0{4}-0{4}-0{12}$/.test(s);
  if (
    !v ||
    !nonzero(v.id) ||
    !Number.isSafeInteger(v.expectedGeneration) ||
    v.expectedGeneration < 1 ||
    !Array.isArray(v.targetIds) ||
    v.targetIds.length < 1 ||
    v.targetIds.length > 64 ||
    !v.targetIds.every(nonzero) ||
    new Set(v.targetIds).size !== v.targetIds.length
  )
    throw new Error(
      "Invalid retained cleanup request. Inspect its server operation before clearing it.",
    );
  return v;
}
