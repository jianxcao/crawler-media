/**
 * 一条 Job **此刻属于哪一档**：要你处理 / 进行中 / 已结束。
 *
 * 从 `task-activity` 里单拎出来，是因为这三个判定必须只有一份实现——侧栏角标、
 * 活动页一级切换器、任务视角内部的状态选项卡都读它，各算各的就会出现同一屏上
 * 侧栏 3、进行中 2 的自相矛盾（那正是 task-activity 存在的理由）。这里不碰
 * React、不发请求，因此可以直接单测。
 */

import type { JobStatus, JobView } from "@/lib/api/jobs";

/**
 * 状态判定集合的元素类型。
 *
 * 新契约 JobStatus 只有 queued/running/succeeded/failed/cancelled 五档；这里保留
 * 旧的 blocked/retry_wait/cancelling/waiting 字面量，是为了兼容仍按旧形状构造
 * 任务对象的单测与存量数据——新契约下这些值永远不会出现。
 */
type JobStatusLike = JobStatus | "blocked" | "retry_wait" | "cancelling" | "waiting";

/** 需要用户判断的 Job 状态；与任务视角「需要处理」同口径。 */
export const ATTENTION_JOB_STATUSES = new Set<JobStatusLike>(["blocked", "failed"]);
export const ACTIVE_FEED_JOB_STATUSES = new Set<JobStatusLike>([
  "queued",
  "running",
  "retry_wait",
  "cancelling",
  "waiting",
]);
export const HISTORY_JOB_STATUSES = new Set<JobStatusLike>(["succeeded", "cancelled"]);

/** 旧 JobView 的兼容字段：新契约 JobDef 不再下发它们（见 lib/api/jobs.ts）。 */
interface LegacyJobFields {
  status?: string | null;
  dismissed_at?: string | null;
}

/** 读取一条 Job 的当前状态：新契约读 last_status，旧形状回退到 status。 */
function jobStatus(job: JobView & LegacyJobFields): JobStatusLike | null {
  return (job.last_status ?? job.status ?? null) as JobStatusLike | null;
}

/**
 * 失败任务是否已被用户忽略。
 *
 * 新契约没有持久化的 dismissed_at（/dismiss 已退化为空操作），忽略是组件层维护
 * 的会话内状态（markJobDismissed / markJobUndismissed，本浏览器持久化）；旧形状
 * 仍按 dismissed_at 字段判断。
 */
export function isDismissed(job: JobView & LegacyJobFields): boolean {
  if (job.dismissed_at != null) return true;
  // 如果任务有最近完成时间戳，严格按 jobId:timestamp 复合键判断，避免忽略一次后永久隐藏未来的新错误；
  // 只有在任务无时间戳时才退回到裸 jobId
  if (job.last_finished_at) {
    return dismissedJobIds.has(`${job.id}:${job.last_finished_at}`);
  }
  return dismissedJobIds.has(job.id);
}

// —— 组件层忽略状态的会话内实现（新契约无 /dismiss 端点，静音语义由前端承担）——

const DISMISSED_STORAGE_KEY = "crawler-media.dismissed.job-ids";
const dismissedJobIds = new Set<string>(loadDismissedIds());

function loadDismissedIds(): string[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(DISMISSED_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

function persistDismissedIds(): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(DISMISSED_STORAGE_KEY, JSON.stringify([...dismissedJobIds]));
  } catch {
    // 隐私模式下写不进：本次会话内仍生效，只是不记住
  }
}

/** 把一条任务标记为用户已忽略（会话内 + 本浏览器持久化）。 */
export function markJobDismissed(jobId: string, finishedAt?: string | number | null): void {
  if (finishedAt) {
    dismissedJobIds.add(`${jobId}:${finishedAt}`);
  } else {
    dismissedJobIds.add(jobId);
  }
  persistDismissedIds();
}

/** 撤销忽略。 */
export function markJobUndismissed(jobId: string, finishedAt?: string | number | null): void {
  if (finishedAt) {
    dismissedJobIds.delete(`${jobId}:${finishedAt}`);
  }
  dismissedJobIds.delete(jobId);
  for (const id of Array.from(dismissedJobIds)) {
    if (id.startsWith(`${jobId}:`)) {
      dismissedJobIds.delete(id);
    }
  }
  persistDismissedIds();
}

/**
 * 这条 Job 是不是**现在**要用户动手。
 *
 * 光看状态不够：`failed` 是终态，取消对它无效，于是它会永远赖在「需要处理」
 * 里，侧栏红角标永不熄灭——用户手上没有任何让它闭嘴的动作（issue #221）。
 * 「忽略」补上了这个出口：任务仍然是失败（日志、重试、事件时间线一概不动），
 * 但用户已经拍板不处理，就不该继续被算成待办。
 */
export function jobNeedsAttention(job: JobView & LegacyJobFields): boolean {
  const status = jobStatus(job);
  return status != null && ATTENTION_JOB_STATUSES.has(status) && !isDismissed(job);
}

/** 已经了结的 Job：正常终态，加上被用户忽略的失败任务。 */
export function jobIsHistorical(job: JobView & LegacyJobFields): boolean {
  const status = jobStatus(job);
  return (
    (status != null && HISTORY_JOB_STATUSES.has(status)) ||
    (status === "failed" && isDismissed(job))
  );
}

/**
 * 判断 Job 是否属于当下正在发生/排队待执行的「活跃任务」：
 * - running / retry_wait / cancelling / waiting 等即时执行状态；
 * - 或者 queued 状态，但调度时间已到（next_run_after <= now 或没有设定延迟），代表正在等 worker 调度。
 * 未到时间的定时计划（next_run_after > now）不属于「进行中」，避免未来定时任务提前污染活动队列。
 */
export function jobIsActive(job: JobView & LegacyJobFields, nowSecs: number = Math.floor(Date.now() / 1000)): boolean {
  const status = jobStatus(job);
  if (!status || !ACTIVE_FEED_JOB_STATUSES.has(status)) {
    return false;
  }
  // 如果是正在运行或取消中等活跃状态，直接属于活跃任务
  if (status !== "queued") {
    return true;
  }
  // 对于 queued 状态，如果存在未来的 next_run_after 调度时间，属于尚未到期的定时任务
  if (job.next_run_after != null && job.next_run_after !== "") {
    const runAfterSecs =
      typeof job.next_run_after === "number"
        ? (job.next_run_after > 1e12 ? Math.floor(job.next_run_after / 1000) : job.next_run_after)
        : Math.floor(new Date(job.next_run_after).getTime() / 1000);
    if (!Number.isNaN(runAfterSecs) && runAfterSecs > nowSecs) {
      return false;
    }
  }
  return true;
}
