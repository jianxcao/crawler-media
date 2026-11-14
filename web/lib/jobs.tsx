"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { ACTIVE_JOB_STATUSES, listJobs, type JobView } from "@/lib/api/jobs";
import { subscribeToJobsStream } from "@/lib/jobs-stream";
import { useSession } from "@/lib/session";

interface JobsContextValue {
  jobs: JobView[];
  activeJobs: JobView[];
  refresh: () => void;
  upsert: (job: JobView) => void;
  latestFor: (resourceType: string, resourceId: string | number, jobType?: string) => JobView | null;
}

const JobsContext = createContext<JobsContextValue | null>(null);
const FALLBACK_POLL_MS = 15_000;

/** 新契约 JobDef 无 updated_at，按最近一次完成/下次运行时间排序（ISO 串字典序即时间序）。 */
function jobRecencyKey(job: JobView): string {
  return String(job.last_finished_at ?? job.next_run_after ?? "");
}

function byRecency(left: JobView, right: JobView): number {
  return jobRecencyKey(right).localeCompare(jobRecencyKey(left));
}

/**
 * 全站唯一后台任务数据源：首次进页面取快照，之后 SSE 即时刷新；反代不支持
 * 流式传输或网络断开时，用低频轮询兜底。任一入口（Web、CLI、Agent、调度器）
 * 创建的任务都会进入这里，业务组件不再各自猜测何时开始轮询。
 */
export function JobsProvider({ children }: { children: React.ReactNode }) {
  const { session } = useSession();
  const enabled = session.role !== "member";
  const [jobs, setJobs] = useState<JobView[]>([]);
  const requestSeq = useRef(0);
  const refreshInFlight = useRef(false);
  const refreshQueued = useRef(false);
  const upsert = useCallback((job: JobView) => {
    setJobs((current) => [job, ...current.filter((item) => item.id !== job.id)].toSorted(byRecency));
  }, []);
  const refresh = useCallback(() => {
    refreshQueued.current = true;
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    void (async () => {
      try {
        // SSE 可能在一个翻译批次内连续推送多条进度。串行排空并把期间的
        // 多次通知合为至多一次补充请求，避免前端制造重叠查询风暴。
        while (refreshQueued.current) {
          refreshQueued.current = false;
          const seq = ++requestSeq.current;
          try {
            // 活跃任务与最近历史并行取：即使近期记录很多，运行较久的旧任务
            // 也不会被新完成记录挤出全局状态。
            const [active, recent] = await Promise.all([
              listJobs({ activeOnly: true, limit: 200 }),
              listJobs({ limit: 50 }),
            ]);
            if (seq !== requestSeq.current) continue;
            const merged = new Map([...recent, ...active].map((job) => [job.id, job]));
            setJobs([...merged.values()].sort(byRecency));
          } catch {
            // 瞬时断线保留最近快照；SSE 重连或下一轮轮询会自动校准。
          }
        }
      } finally {
        refreshInFlight.current = false;
      }
    })();
  }, []);

  useEffect(() => {
    if (!enabled) {
      setJobs([]);
      return;
    }
    refresh();
    const timer = window.setInterval(refresh, FALLBACK_POLL_MS);
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);

    const unsubscribe = subscribeToJobsStream(refresh);

    return () => {
      requestSeq.current += 1;
      refreshQueued.current = false;
      unsubscribe();
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [enabled, refresh, session.username]);

  const activeJobs = useMemo(
    () => jobs.filter((job) => job.last_status != null && ACTIVE_JOB_STATUSES.has(job.last_status)),
    [jobs],
  );
  const latestFor = useCallback(
    (_resourceType: string, _resourceId: string | number, jobType?: string) => {
      // 新契约 JobDef 没有 resources，无法按资源关联任务；退化为按任务类型取最近一条
      //（resources 关联语义随旧契约下线，本方法已无调用方）。
      return jobs.find((job) => !jobType || job.kind === jobType) ?? null;
    },
    [jobs],
  );
  const value = useMemo(
    () => ({ jobs, activeJobs, refresh, upsert, latestFor }),
    [jobs, activeJobs, refresh, upsert, latestFor],
  );
  return <JobsContext.Provider value={value}>{children}</JobsContext.Provider>;
}

export function useJobs(): JobsContextValue {
  const value = useContext(JobsContext);
  if (!value) throw new Error("useJobs 必须在 JobsProvider 内使用");
  return value;
}
