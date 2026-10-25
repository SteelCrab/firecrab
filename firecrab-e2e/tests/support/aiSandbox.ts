export interface ModelSelection { connectionId: string; model: string }
export interface SandboxModel { id: string; name: string; verified: boolean; toolsVerified: boolean }
export interface ModelCatalog { models: SandboxModel[]; discoveryAvailable: boolean }
export interface ProjectSpec { name: string; sourcePath: string | null; vmId: string | null; selection: ModelSelection | null; instructions: string }
export interface Project { id: string; spec: ProjectSpec; createdAt: number }
export type TaskState = "waiting" | "accepted" | "in_progress" | "result" | "merged" | "failed" | "cancelled" | "interrupted";
export interface TaskInput { kind: "chat" | "code"; input: string; selection: ModelSelection | null; retryOf: string | null }
export interface ToolEvent { sequence: number; at: number; name: string; input: string; output: string; stderr: string; exitCode: number | null }
export interface FileChange { path: string; before: string | null; after: string | null; beforeMode: number | null; afterMode: number | null; applied: boolean }
export interface SandboxTask { id: string; projectId: string; attemptId: string; request: TaskInput; selection: ModelSelection; state: TaskState; createdAt: number; updatedAt: number; result: string; errorCode: string | null; error: string | null; events: ToolEvent[]; changes: FileChange[] }
export type TaskSummary = Omit<SandboxTask, "events" | "changes"> & { eventCount: number; changeCount: number; lastTool: string | null };
