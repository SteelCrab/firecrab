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
