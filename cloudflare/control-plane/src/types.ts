export type TaskId = string;
export type Attempt = number;
export type RemoteRunId = string;

export type TaskStatus =
  | "queued"
  | "booting"
  | "cloning"
  | "running"
  | "pushing"
  | "completed"
  | "failed"
  | "cancelled";

export const terminalStatuses = new Set<TaskStatus>([
  "completed",
  "failed",
  "cancelled",
]);

export interface RepoSpec {
  provider: "github";
  owner: string;
  name: string;
  branch: string;
}

export interface InstructionSpec {
  prompt: string;
  working_dir: string;
  model: string;
  worker_runner: WorkerRunnerSpec;
  after_create_hook?: string;
  before_run_hook?: string;
  after_run_hook?: string;
}

export interface WorkerRunnerSpec {
  runner: "pi" | "opencode";
  extensions: string[];
}

export interface ExecutionPolicy {
  max_duration_seconds: number;
  allow_network: boolean;
}

export interface StartTaskRequest {
  task_id: TaskId;
  attempt: Attempt;
  repo: RepoSpec;
  instruction: InstructionSpec;
  execution_policy: ExecutionPolicy;
}

export interface StartTaskResponse {
  remote_run_id: RemoteRunId;
  status: TaskStatus;
}

export interface TaskStatusResponse {
  remote_run_id: RemoteRunId;
  status: TaskStatus;
  phase: TaskStatus;
  updated_at: string;
}

export type LogStream = "stdout" | "stderr";

export interface LogLine {
  index: number;
  ts: string;
  stream: LogStream;
  message: string;
}

export interface LogsResponse {
  remote_run_id: RemoteRunId;
  lines: LogLine[];
  next_cursor: number;
  has_more: boolean;
}

export interface TaskResult {
  commit_sha?: string;
  branch: string;
  summary: string;
  report?: ResearchReport;
}

export interface ResearchReport {
  goal_id: string;
  run_id: string;
  variant: string;
  summary: string;
  findings?: Array<{
    claim: string;
    evidence: string;
    source?: string | null;
    confidence: "low" | "medium" | "high";
  }>;
  recommendation: string;
  risks?: string[];
  follow_up_questions?: string[];
  suggested_tasks?: Array<{
    title: string;
    acceptance_criteria: string;
    context: string;
  }>;
}

export interface TaskError {
  code: string;
  message: string;
}

export interface TaskResultResponse {
  remote_run_id: RemoteRunId;
  status: TaskStatus;
  result?: TaskResult;
  error?: TaskError;
}

export interface CancelTaskResponse {
  remote_run_id: RemoteRunId;
  status: TaskStatus;
}

export interface ErrorResponse {
  error: TaskError;
}

export interface TaskState {
  remote_run_id: RemoteRunId;
  task?: StartTaskRequest;
  status: TaskStatus;
  phase: TaskStatus;
  logs: LogLine[];
  next_log_index: number;
  result?: TaskResult;
  error?: TaskError;
  cancelled: boolean;
  started: boolean;
  created_at: string;
  updated_at: string;
}

export function remoteRunId(taskId: TaskId, attempt: Attempt): RemoteRunId {
  return `cf-${taskId}-${attempt}`;
}

export function durableObjectName(runId: RemoteRunId): string {
  return `task-${runId.replace(/^cf-/, "")}`;
}
