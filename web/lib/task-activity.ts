"use client";

/**
 * 任务活动的**唯一计数口径**。
 *
 * 侧栏「活动」角标、活动页一级切换器上的「任务」提示、任务视角内部的状态
 * 选项卡，三处说的都得是同一件事：这个系统现在有几件任务要你看。
 *
 * 因此把分组与归类下沉到这里：谁要计数就调 `useTaskActivity()`，谁要渲染就
 * 拿同一份分组结果去画。本模块只做归类与后台探测队列快照共享，不碰下载器或
 * Job 表的领域状态。
 */

import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import type { DownloadTask } from "@/lib/api/downloaders";
import type { JobView } from "@/lib/api/jobs";
import {
  cancelProbeQueue,
  getProbeStatus,
  pauseProbeQueue,
  resumeProbeQueue,
  type ProbeCancelReceipt,
  type ProbeQueueStatus,
} from "@/lib/api/probe";
import {
  downloadGroupNeedsAttention,
  groupDownloadTasks,
  isBoostTask,
  type DownloadTaskGroup,
} from "@/lib/download-attention";
import { useDownloadTasks } from "@/lib/download-tasks";
import {
  jobIsActive,
  jobIsHistorical,
  jobNeedsAttention,
} from "@/lib/job-attention";
import { useJobs } from "@/lib/jobs";
import { useSession } from "@/lib/session";
import { useVisiblePolling } from "@/lib/use-visible-polling";

export {
  downloadGroupNeedsAttention,
  downloadTaskNeedsAttention,
  groupDownloadTasks,
  isBoostTask,
  isSubscriptionTask,
  taskSourceOf,
  type DownloadTaskGroup,
} from "@/lib/download-attention";

const GLOBAL_PROBE_POLL_MS = 15_000;
const ACTIVITY_PROBE_POLL_MS = 5_000;

export function probeQueueHasWork(status: ProbeQueueStatus | null | undefined): boolean {
  if (!status) return false;
  return (
    status.total_pending > 0 ||
    status.running_metadata + status.running_fingerprint > 0
  );
}

export function probeQueueCountsAsActive(status: ProbeQueueStatus | null | undefined): boolean {
  if (!status) return false;
  return !status.paused && probeQueueHasWork(status);
}

export interface ProbeQueueContextValue {
  status: ProbeQueueStatus | null;
  busyAction: "pause" | "resume" | "cancel" | null;
  refresh: () => void;
  pause: () => Promise<ProbeQueueStatus>;
  resume: () => Promise<ProbeQueueStatus>;
  cancel: (libraryId?: string) => Promise<ProbeCancelReceipt>;
  retainFastPolling: () => () => void;
}

const DEFAULT_PROBE_QUEUE: ProbeQueueContextValue = {
  status: null,
  busyAction: null,
  refresh: () => undefined,
  pause: async () => {
    throw new Error("ProbeQueueProvider 未挂载");
  },
  resume: async () => {
    throw new Error("ProbeQueueProvider 未挂载");
  },
  cancel: async () => {
    throw new Error("ProbeQueueProvider 未挂载");
  },
  retainFastPolling: () => () => undefined,
};

const ProbeQueueContext = createContext<ProbeQueueContextValue>(DEFAULT_PROBE_QUEUE);

export function ProbeQueueProvider({ children }: { children: ReactNode }) {
  const { session } = useSession();
  const enabled = session.role === "admin";
  const [status, setStatus] = useState<ProbeQueueStatus | null>(null);
  const [busyAction, setBusyAction] = useState<"pause" | "resume" | "cancel" | null>(null);
  const [fastCount, setFastCount] = useState(0);
  const inFlight = useRef(false);
  const queued = useRef(false);
  const mutationSeqRef = useRef(0);

  const refresh = useCallback(() => {
    if (!enabled) {
      setStatus(null);
      return;
    }
    queued.current = true;
    if (inFlight.current) return;
    inFlight.current = true;
    void (async () => {
      try {
        while (queued.current) {
          queued.current = false;
          const seqAtStart = mutationSeqRef.current;
          try {
            const next = await getProbeStatus();
            if (mutationSeqRef.current !== seqAtStart) {
              queued.current = true;
              continue;
            }
            setStatus(next);
          } catch {
            // 瞬时网络波动保留上一次快照，下一轮轮询恢复
          }
        }
      } finally {
        inFlight.current = false;
      }
    })();
  }, [enabled]);

  const intervalMs = enabled
    ? fastCount > 0
      ? ACTIVITY_PROBE_POLL_MS
      : GLOBAL_PROBE_POLL_MS
    : null;
  useVisiblePolling(refresh, intervalMs, { leading: true });

  const pause = useCallback(async () => {
    mutationSeqRef.current += 1;
    setBusyAction("pause");
    try {
      const next = await pauseProbeQueue();
      mutationSeqRef.current += 1;
      setStatus(next);
      return next;
    } finally {
      setBusyAction(null);
    }
  }, []);

  const resume = useCallback(async () => {
    mutationSeqRef.current += 1;
    setBusyAction("resume");
    try {
      const next = await resumeProbeQueue();
      mutationSeqRef.current += 1;
      setStatus(next);
      return next;
    } finally {
      setBusyAction(null);
    }
  }, []);

  const cancel = useCallback(
    async (libraryId?: string) => {
      mutationSeqRef.current += 1;
      setBusyAction("cancel");
      try {
        const receipt = await cancelProbeQueue(libraryId);
        mutationSeqRef.current += 1;
        const seqAfterCancel = mutationSeqRef.current;
        const next = await getProbeStatus();
        if (mutationSeqRef.current === seqAfterCancel) {
          setStatus(next);
        }
        return receipt;
      } finally {
        setBusyAction(null);
      }
    },
    [],
  );

  const retainFastPolling = useCallback(() => {
    setFastCount((count) => count + 1);
    return () => setFastCount((count) => Math.max(0, count - 1));
  }, []);

  const value = useMemo<ProbeQueueContextValue>(
    () => ({
      status: enabled ? status : null,
      busyAction,
      refresh,
      pause,
      resume,
      cancel,
      retainFastPolling,
    }),
    [busyAction, cancel, enabled, pause, refresh, retainFastPolling, resume, status],
  );

  return createElement(ProbeQueueContext.Provider, { value }, children);
}

export function useProbeQueue(): ProbeQueueContextValue {
  return useContext(ProbeQueueContext);
}

export interface TaskActivity {
  /** infohash（小写）→ 关联的入库 Job：下载与入库是同一件事的两段。 */
  ingestJobsByHash: Map<string, JobView>;
  attentionDownloadGroups: DownloadTaskGroup[];
  activeDownloadGroups: DownloadTaskGroup[];
  /** 刷流做种：没有入库/救援语义，既不进时间线也不参与任何计数。 */
  boostTasks: DownloadTask[];
  standaloneAttentionJobs: JobView[];
  standaloneActiveJobs: JobView[];
  standaloneHistoricalJobs: JobView[];
  probeStatus: ProbeQueueStatus | null;
  probeQueue?: ProbeQueueContextValue;
  /** 需要处理的条数（下载过程按作品合并后计一条）。 */
  attentionTotal: number;
  /** 进行中的条数。 */
  activeTotal: number;
  historyTotal: number;
}

/**
 * 汇总当前的任务活动。数据来自全站唯一的 Provider（Job / 下载快照 / 媒体探测状态），
 * 本 Hook 不发起任何请求，因此侧栏与活动页同时使用也不会重复请求。
 */
export function useTaskActivity(): TaskActivity {
  const { jobs } = useJobs();
  const { tasks: downloadTasks } = useDownloadTasks();
  const probeQueue = useProbeQueue();
  const probeStatus = probeQueue.status;

  const ingestJobsByHash = useMemo(() => new Map<string, JobView>(), []);

  const boostTasks = useMemo(
    () => downloadTasks.filter(isBoostTask),
    [downloadTasks],
  );
  const mediaTasks = useMemo(
    () => downloadTasks.filter((task) => !isBoostTask(task)),
    [downloadTasks],
  );
  const downloadGroups = useMemo(() => groupDownloadTasks(mediaTasks), [mediaTasks]);
  const attentionDownloadGroups = useMemo(
    () => downloadGroups.filter((group) => downloadGroupNeedsAttention(group, ingestJobsByHash)),
    [downloadGroups, ingestJobsByHash],
  );
  const activeDownloadGroups = useMemo(
    () => downloadGroups.filter((group) => !downloadGroupNeedsAttention(group, ingestJobsByHash)),
    [downloadGroups, ingestJobsByHash],
  );

  const linkedIngestJobIds = useMemo(
    () =>
      new Set(
        downloadTasks
          .map((task) =>
            task.info_hash ? ingestJobsByHash.get(task.info_hash.toLowerCase())?.id : undefined,
          )
          .filter((jobId): jobId is string => jobId != null),
      ),
    [downloadTasks, ingestJobsByHash],
  );
  const standaloneAttentionJobs = useMemo(
    () => jobs.filter((job) => jobNeedsAttention(job) && !linkedIngestJobIds.has(job.id)),
    [jobs, linkedIngestJobIds],
  );
  const standaloneActiveJobs = useMemo(
    () => jobs.filter((job) => jobIsActive(job) && !linkedIngestJobIds.has(job.id)),
    [jobs, linkedIngestJobIds],
  );
  const standaloneHistoricalJobs = useMemo(
    () => jobs.filter((job) => jobIsHistorical(job) && !linkedIngestJobIds.has(job.id)),
    [jobs, linkedIngestJobIds],
  );

  const probeActiveCount = probeQueueCountsAsActive(probeStatus) ? 1 : 0;

  return {
    ingestJobsByHash,
    attentionDownloadGroups,
    activeDownloadGroups,
    boostTasks,
    standaloneAttentionJobs,
    standaloneActiveJobs,
    standaloneHistoricalJobs,
    probeStatus,
    probeQueue,
    attentionTotal: attentionDownloadGroups.length + standaloneAttentionJobs.length,
    activeTotal:
      activeDownloadGroups.length + standaloneActiveJobs.length + probeActiveCount,
    historyTotal: standaloneHistoricalJobs.length,
  };
}

export interface TaskActivityBadge {
  alert: boolean;
  count: number;
  href: string;
  hint: string;
}

export function taskActivityBadge(activity: TaskActivity): TaskActivityBadge {
  const alert = activity.attentionTotal > 0;
  if (alert) {
    return {
      alert: true,
      count: activity.attentionTotal,
      href: "/activity?view=attention",
      hint: `${activity.attentionTotal} 项需要处理`,
    };
  }
  return {
    alert: false,
    count: activity.activeTotal,
    href: activity.activeTotal > 0 ? "/activity?view=active" : "/activity",
    hint: activity.activeTotal > 0 ? `${activity.activeTotal} 个进行中` : "暂无进行中的任务",
  };
}
