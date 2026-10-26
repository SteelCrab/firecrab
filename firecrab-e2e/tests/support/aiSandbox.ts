export interface ModelSelection { connectionId: string; model: string }
export interface SandboxModel { id: string; name: string; verified: boolean; toolsVerified: boolean }
export interface ModelCatalog { models: SandboxModel[]; discoveryAvailable: boolean }
export interface ProjectSpec { name: string; sourcePath: string | null; vmId: string | null; environmentId?: string | null; selection: ModelSelection | null; instructions: string; context?: { gitRef: string | null; selectedSkills: string[]; references: string[] } }
export interface Project { id: string; spec: ProjectSpec; createdAt: number }
export type TaskState = "waiting" | "accepted" | "in_progress" | "result" | "merged" | "failed" | "cancelled" | "interrupted";
export interface TaskInput { kind: "chat" | "code"; input: string; selection: ModelSelection | null; retryOf: string | null; coordination?: { dependencies: string[]; assignedAgent: string | null; retryLimit: number }; environmentRole?: string | null }
export interface ToolEvent { sequence: number; at: number; name: string; input: string; output: string; stderr: string; exitCode: number | null }
export interface FileChange { path: string; before: string | null; after: string | null; beforeMode: number | null; afterMode: number | null; applied: boolean }
export interface SandboxTask { id: string; projectId: string; attemptId: string; runtime?: {vmId: string; runtimeId: string} | null; environment?: {id: string; generation: string; role: string} | null; request: TaskInput; selection: ModelSelection; state: TaskState; createdAt: number; updatedAt: number; result: string; errorCode: string | null; error: string | null; events: ToolEvent[]; changes: FileChange[]; tests?: {name: string; eventSequence: number; passed: boolean}[]; artifacts?: {id: string; taskId: string; attemptId: string; path: string; size: number; sha256: string}[] }
export type TaskSummary = Omit<SandboxTask, "events" | "changes"> & { eventCount: number; changeCount: number; lastTool: string | null };
