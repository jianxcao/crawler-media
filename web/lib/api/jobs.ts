import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/**
 * 任务状态（自有契约 Job.status 枚举的投影；定义见 docs/api-contracts/self.md）。
 * 调度层把「排队/执行中」视为活跃，其余为终态。
 */
export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled";

/**
 * 任务定义（docs/api-contracts/self.md 的 JobDef）：调度层的持久化定义。
 * 前端任务中心以它为视图模型；运行中/最近一次运行状态见 last_* 字段。
 */
export interface JobDef {
  id: string;
  kind: string;
  name: string;
  friendly_name?: string;
  enabled: boolean;
  schedule: { interval_secs: number } | null;
  payload: Record<string, unknown>;
  concurrency_key: string | null;
  last_status: JobStatus | null;
  last_finished_at: string | number | null;
  last_error?: string | null;
  next_run_after: string | number | null;
}

/** 任务中心的视图模型 = 任务定义（组件引用名保持旧名 JobView）。 */
export type JobView = JobDef;

/** 活跃（未终结）任务状态集合：用于轮询「还有没有任务在跑」。 */
export const ACTIVE_JOB_STATUSES = new Set<JobStatus>(["queued", "running"]);

/** 是否系统收口的取消：新契约无 cancel_requested_by，仅能按终态判断。 */
export function isSystemCancelled(job: JobView): boolean {
  return job.last_status === "cancelled";
}

/** 列出任务定义（可按 scope=system/all 和 limit 过滤）。 */
export async function listJobs(
  options: {
    scope?: "system" | "all";
    activeOnly?: boolean;
    limit?: number;
  } = {},
): Promise<JobView[]> {
  const query = new URLSearchParams();
  if (options.scope) query.set("scope", options.scope);
  if (options.activeOnly !== undefined) query.set("active_only", String(options.activeOnly));
  if (options.limit !== undefined) query.set("limit", String(options.limit));
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<JobDef[]>>(`/jobs${suffix}`));
}

/** 读取单个任务定义。 */
export function getJob(jobId: string): Promise<JobView> {
  return unwrap(request<ApiEnvelope<JobDef>>(`/jobs/${jobId}`));
}

/** 切换任务启用/停用状态。 */
export function toggleJob(jobId: string, enabled: boolean): Promise<{ id: string; enabled: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ id: string; enabled: boolean }>>(`/jobs/${jobId}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
      headers: { "Content-Type": "application/json" },
    }),
  );
}

/** 修改任务执行周期（秒）。 */
export function updateJobSchedule(
  jobId: string,
  intervalSecs: number | null,
): Promise<{ id: string; schedule: { interval_secs: number } | null }> {
  return unwrap(
    request<ApiEnvelope<{ id: string; schedule: { interval_secs: number } | null }>>(
      `/jobs/${jobId}/schedule`,
      {
        method: "PUT",
        body: JSON.stringify({ interval_secs: intervalSecs }),
        headers: { "Content-Type": "application/json" },
      },
    ),
  );
}

/** 立即执行一个任务（跳过调度等待）；任务快照由调用方随后刷新。 */
export function runJob(jobId: string): Promise<{ queued: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean }>>(`/jobs/${jobId}/run`, { method: "POST" }),
  );
}

/** 取消排队/运行中的任务；任务快照由调用方随后刷新。 */
export function cancelJob(jobId: string): Promise<{ cancelled: number }> {
  return unwrap(
    request<ApiEnvelope<{ cancelled: number }>>(`/jobs/${jobId}/cancel`, { method: "POST" }),
  );
}

/** 重新执行：新契约无 /retry 端点，等价于立即 run 一次。 */
export function retryJob(jobId: string): Promise<{ queued: boolean }> {
  return runJob(jobId);
}

/**
 * 忽略一条失败任务：新契约无 /dismiss 端点，退化为读取当前定义
 * （组件依赖的返回类型 JobView 不变；静音语义由组件适配阶段另行处理）。
 */
export async function dismissJob(jobId: string, muteSource = false): Promise<JobView> {
  return getJob(jobId);
}

/** 取消「已忽略」：同上，退化为读取当前定义。 */
export async function undismissJob(jobId: string): Promise<JobView> {
  return getJob(jobId);
}

/** 收口失败任务：无批量端点，返回当前处于 failed 的定义列表。 */
export async function dismissAllFailedJobs(): Promise<JobView[]> {
  const jobs = await listJobs();
  return jobs.filter((job) => job.last_status === "failed");
}

/** 手动触发一次调度 tick（开发用）。 */
export function tickJobs(): Promise<Record<string, unknown>> {
  return unwrap(
    request<ApiEnvelope<Record<string, unknown>>>("/jobs/tick", { method: "POST" }),
  );
}
