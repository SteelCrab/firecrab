export interface BenchmarkRunMetadata {
  run_id: string;
  commit_sha: string;
  branch: string;
  dirty?: boolean;
  timestamp: string;
  host: string;
  firecrab_version: string;
  kernel_version: string;
}

export interface BenchmarkLatency {
  average_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  minimum_ms: number;
  maximum_ms: number;
}

export interface BenchmarkResult {
  schema_version: number;
  run: BenchmarkRunMetadata;
  test: string;
  requested_count: number;
  attempted_count: number;
  successful_count: number;
  failed_count: number;
  failure_rate: number;
  latency?: BenchmarkLatency;
  failures?: string[];
  metrics?: Record<string, number>;
  host_resources: {
    cpu_percent: number | null;
    memory_used_mib: number | null;
    memory_total_mib: number | null;
    load_average_1m: number | null;
    firecracker_process_count: number | null;
  };
}

export type BenchmarkJobCommand = "boot" | "create" | "density" | "lifecycle";
export type BenchmarkJobStatus = "running" | "cancelling" | "succeeded" | "failed" | "cancelled";

export interface StartBenchmarkJobRequest {
  command: BenchmarkJobCommand;
  template: string;
  microNetworkId: string;
  ram: number;
  cpu: number;
  diskGb: number;
  count?: number;
  concurrency?: number;
  maxVms?: number;
  step?: number;
  iterations?: number;
  confirmDensity?: boolean;
}

export interface BenchmarkJob {
  id: string;
  request: StartBenchmarkJobRequest;
  status: BenchmarkJobStatus;
  createdAtMs: number;
  finishedAtMs?: number;
  log: string;
}
