export interface SecurityFinding {
  cve: string;
  package?: string;
  installedVersion?: string;
  fixedVersion?: string;
  severity?: string;
  advisorySource?: string;
  target?: string;
}
export interface SecurityScan {
  status?: string;
  findings?: SecurityFinding[];
  deploymentId?: string;
  digest?: string;
  scannedAt?: number;
  databaseUpdatedAt?: string;
  limitations?: string[];
  replicaIds?: string[];
}
export interface SecurityActivity {
  source?: string;
  count?: number;
  severity?: string;
  recommendation?: string;
  deploymentId?: string;
  replicaIds?: string[];
  bucketStart?: number;
}
export interface SecurityService {
  id: string;
  name: string;
  deploymentId?: string;
  digest?: string | null;
  replicaIds?: string[];
  sampledRecords?: number;
  logGap?: boolean;
  scan?: SecurityScan | null;
  previousScans?: SecurityScan[];
  signals?: SecurityActivity[];
  activityHistory?: SecurityActivity[];
}
export interface SecurityCoverage {
  services?: SecurityService[];
  unsupportedWorkloads?: { id: string; name: string; reason: string }[];
  databases?: {
    id: string;
    name: string;
    serviceId?: string | null;
    serviceName?: string | null;
  }[];
  ingressLogs?: string;
}
export interface SecurityModel {
  connectionId: string;
  model: string;
  account: string;
  name: string;
}
export interface SecurityExplanation {
  serviceId: string;
  deploymentId: string;
  text: string;
}

/** Preserve every word; bound reading lines without splitting version strings. */
export function readingLines(message: string): string[] {
  const lines: string[] = [];
  let words: string[] = [];
  for (const word of message.trim().split(/\s+/).filter(Boolean)) {
    words.push(word);
    if (words.length === 8 || /[.!?。！？]$/.test(word)) {
      lines.push(words.join(" "));
      words = [];
    }
  }
  if (words.length) lines.push(words.join(" "));
  return lines;
}
export function severityRank(severity?: string): number {
  return (
    ({ critical: 0, high: 1, medium: 2, low: 3 } as Record<string, number>)[
      severity ?? ""
    ] ?? 4
  );
}
