"use client";

import { useEffect, useRef, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import { JobCard } from "@/components/job-center";
import { ScheduledJobsView } from "@/components/scheduled-jobs-view";
import {
  deleteDownloadTask,
  replaceDownloadTask,
  type DownloadTask,
} from "@/lib/api/downloaders";
import { cancelJob, listJobs, runJob, type JobView } from "@/lib/api/jobs";
import { useDownloadTasks } from "@/lib/download-tasks";
import { markJobDismissed, markJobUndismissed } from "@/lib/job-attention";
import { useJobs } from "@/lib/jobs";
import { useTaskActivity } from "@/lib/task-activity";
import type { TaskCenterViewName } from "@/lib/task-center";
import { formatRelativeTime } from "@/lib/time";

import { BoostTaskSection } from "./task-center/boost-task-section";
import { downloadGroupTimelineTone } from "./task-center/download-lifecycle";
import {
  DownloadTaskGroupCard,
  DownloadTaskGroupFeed,
} from "./task-center/download-task-cards";
import {
  ActiveJobFeedItem,
  jobTimelineTone,
  TaskHistorySection,
} from "./task-center/job-feed-items";
import { ProbeQueueCard } from "./task-center/probe-queue-card";
import {
  DeleteDownloadTaskDialog,
  EmptyView,
  SourceWarning,
  TaskAttentionSection,
  TaskTimelineItem,
  TaskTimelineSection,
} from "./task-center/task-layout";

const VIEW_LABELS: { id: TaskCenterViewName; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "active", label: "进行中" },
  { id: "attention", label: "需要处理" },
  { id: "scheduled", label: "定时计划" },
  { id: "history", label: "已结束" },
];

export function TaskCenterView({
  view,
  onViewChange,
}: {
  view: TaskCenterViewName;
  onViewChange: (view: TaskCenterViewName) => void;
}) {
  const setView = onViewChange;
  const activeTabRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    activeTabRef.current?.scrollIntoView({ block: "nearest", inline: "center" });
  }, [view]);
  const [deletingTaskId, setDeletingTaskId] = useState<string | null>(null);
  const [replacingTaskId, setReplacingTaskId] = useState<string | null>(null);
  const [cancellingJobId, setCancellingJobId] = useState<string | null>(null);
  const [retryingJobId, setRetryingJobId] = useState<string | null>(null);
  const [bulkDismissing, setBulkDismissing] = useState(false);
  const [undismissingJobId, setUndismissingJobId] = useState<string | null>(null);
  const [pendingDeleteTask, setPendingDeleteTask] = useState<DownloadTask | null>(null);
  const toast = useToast();
  const { refresh: refreshJobs, upsert } = useJobs();
  const { loading, error, refreshedAt, refresh } = useDownloadTasks();
  const activity = useTaskActivity();
  const probeQueue = activity.probeQueue;
  const retainFastPolling = probeQueue?.retainFastPolling;

  useEffect(() => {
    return retainFastPolling?.();
  }, [retainFastPolling]);

  async function removeDownloadTask(task: DownloadTask, deleteFiles: boolean) {
    if (deletingTaskId != null) return;
    setDeletingTaskId(task.id);
    try {
      const result = await deleteDownloadTask(task.id, deleteFiles);
      toast.success(
        result.delete_files ? "种子任务和数据文件已删除" : "种子任务已删除，数据文件已保留",
      );
      setPendingDeleteTask(null);
      refresh();
    } catch (caught) {
      toast.error((caught as Error).message || "删除种子任务失败");
    } finally {
      setDeletingTaskId(null);
    }
  }

  async function replaceStalledTask(task: DownloadTask) {
    if (replacingTaskId != null) return;
    setReplacingTaskId(task.id);
    try {
      await replaceDownloadTask(task.id);
      toast.success("已开始寻找同品质替代源；旧任务会保留到新源产生真实进度");
      refresh();
    } catch (caught) {
      toast.error((caught as Error).message || "立即换种失败");
    } finally {
      setReplacingTaskId(null);
    }
  }

  async function cancelBackgroundJob(job: JobView) {
    if (cancellingJobId != null) return;
    setCancellingJobId(job.id);
    try {
      await cancelJob(job.id);
      refreshJobs();
      toast.success("已提交取消请求");
    } catch (caught) {
      toast.error((caught as Error).message || "取消任务失败");
    } finally {
      setCancellingJobId(null);
    }
  }

  async function dismissAllFailed() {
    if (bulkDismissing) return;
    setBulkDismissing(true);
    try {
      const dismissed = (await listJobs()).filter((job) => job.last_status === "failed");
      for (const job of dismissed) {
        upsert(job);
        markJobDismissed(job.id, job.last_finished_at);
      }
      toast.success(
        dismissed.length > 0
          ? `已忽略 ${dismissed.length} 个失败任务`
          : "当前没有需要忽略的失败任务",
      );
    } catch (caught) {
      toast.error((caught as Error).message || "批量忽略失败");
    } finally {
      setBulkDismissing(false);
    }
  }

  async function restoreDismissedJob(job: JobView) {
    if (undismissingJobId != null) return;
    setUndismissingJobId(job.id);
    try {
      markJobUndismissed(job.id);
      upsert(job);
      toast.success("已撤销忽略，任务回到「需要处理」");
    } catch (caught) {
      toast.error((caught as Error).message || "撤销忽略失败");
    } finally {
      setUndismissingJobId(null);
    }
  }

  async function retryHistoricalJob(job: JobView) {
    if (retryingJobId != null) return;
    setRetryingJobId(job.id);
    try {
      await runJob(job.id);
      refreshJobs();
      toast.success("任务已重新加入队列");
    } catch (caught) {
      toast.error((caught as Error).message || "重新执行失败");
    } finally {
      setRetryingJobId(null);
    }
  }

  async function handlePauseProbe() {
    if (!probeQueue) return;
    try {
      await probeQueue.pause();
      toast.success("后台批量媒体探测已暂停");
    } catch (caught) {
      toast.error((caught as Error).message || "暂停探测队列失败");
    }
  }

  async function handleResumeProbe() {
    if (!probeQueue) return;
    try {
      await probeQueue.resume();
      toast.success("已恢复后台媒体探测队列");
    } catch (caught) {
      toast.error((caught as Error).message || "继续执行探测失败");
    }
  }

  async function handleCancelProbe(libraryId?: string) {
    if (!probeQueue) return;
    try {
      const receipt = await probeQueue.cancel(libraryId);
      toast.success(
        `已清空待探测队列（取消 ${receipt.cancelled_units} 项，清理 ${receipt.cleared_obligations} 条待办）`,
      );
    } catch (caught) {
      toast.error((caught as Error).message || "清空探测队列失败");
    }
  }

  const {
    ingestJobsByHash,
    attentionDownloadGroups,
    activeDownloadGroups,
    boostTasks,
    standaloneAttentionJobs,
    standaloneActiveJobs,
    standaloneHistoricalJobs,
    attentionTotal,
    activeTotal,
  } = activity;
  const probeStatus = activity.probeStatus ?? probeQueue?.status ?? null;
  const hasProbeCard =
    probeStatus != null &&
    (probeStatus.total_pending > 0 ||
      probeStatus.running_metadata + probeStatus.running_fingerprint > 0);
  const probeIsActive = hasProbeCard && !probeStatus.paused;
  const timelineActiveCount =
    activeDownloadGroups.length + standaloneActiveJobs.length;
  const showAttention = view === "all" || view === "attention";
  const showActive = view === "all" || view === "active";
  const showHistory = view === "all" || view === "history";
  const hasContentBeforeHistory =
    (showAttention && attentionTotal > 0) ||
    (showActive && (timelineActiveCount > 0 || hasProbeCard));
  const visibleCount =
    (showAttention ? attentionTotal : 0) +
    (showActive ? timelineActiveCount + (hasProbeCard ? 1 : 0) : 0) +
    (showActive && boostTasks.length > 0 ? 1 : 0) +
    (showHistory ? standaloneHistoricalJobs.length : 0);
  const viewCounts: Partial<Record<TaskCenterViewName, number>> = {
    attention: attentionTotal,
    active: timelineActiveCount + (probeIsActive ? 1 : 0),
    history: standaloneHistoricalJobs.length,
  };

  return (
    <>
      {error && view !== "scheduled" && <SourceWarning error={error} />}

      <div className="mt-6 flex items-center gap-3 border-b border-white/[0.08] max-md:mt-5">
        <div className="scroll-thin flex min-w-0 flex-1 gap-1 overflow-x-auto pb-2">
          {VIEW_LABELS.map((item) => (
            <button
              key={item.id}
              type="button"
              ref={view === item.id ? activeTabRef : undefined}
              aria-pressed={view === item.id}
              onClick={() => setView(item.id)}
              className={`shrink-0 rounded-full px-3.5 py-1.5 text-ui font-medium transition ${
                view === item.id
                  ? "bg-white/[0.14] text-white"
                  : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-white"
              }`}
            >
              {item.label}
              {item.id !== "all" && item.id !== "scheduled" && (viewCounts[item.id] ?? 0) > 0 && (
                <span className="tnum ml-1.5 text-caption text-white/45">
                  {viewCounts[item.id]}
                </span>
              )}
            </button>
          ))}
        </div>
        {view !== "scheduled" && (
          <p className="shrink-0 pb-2 text-caption text-[var(--text-faint)] max-md:hidden">
            {refreshedAt
              ? `${formatRelativeTime(new Date(refreshedAt).toISOString())}更新`
              : loading
                ? "正在读取下载器"
                : "等待刷新"}
          </p>
        )}
      </div>

      {view === "scheduled" ? (
        <ScheduledJobsView embedded />
      ) : (
        <>
          {showAttention && attentionTotal > 0 && (
            <TaskAttentionSection
              count={attentionTotal}
              canDismissAll={standaloneAttentionJobs.some((job) => job.last_status === "failed")}
              dismissingAll={bulkDismissing}
              onDismissAll={() => void dismissAllFailed()}
            >
              {attentionDownloadGroups.map((group) => (
                <DownloadTaskGroupCard
                  key={group.key}
                  group={group}
                  ingestJobsByHash={ingestJobsByHash}
                  deletingTaskId={deletingTaskId}
                  replacingTaskId={replacingTaskId}
                  onDelete={setPendingDeleteTask}
                  onReplace={(task) => void replaceStalledTask(task)}
                />
              ))}
              {standaloneAttentionJobs.map((job) => (
                <JobCard key={job.id} job={job} onNavigate={() => undefined} />
              ))}
            </TaskAttentionSection>
          )}

          {showActive && probeStatus && hasProbeCard && (
            <ProbeQueueCard
              status={probeStatus}
              busyAction={probeQueue?.busyAction ?? null}
              onPause={() => void handlePauseProbe()}
              onResume={() => void handleResumeProbe()}
              onCancel={(libraryId) => void handleCancelProbe(libraryId)}
            />
          )}

          {showActive && timelineActiveCount > 0 && (
            <TaskTimelineSection title="现在" count={activeTotal}>
              {activeDownloadGroups.map((group) => (
                <TaskTimelineItem
                  key={group.key}
                  tone={downloadGroupTimelineTone(group, ingestJobsByHash)}
                >
                  <DownloadTaskGroupFeed
                    group={group}
                    ingestJobsByHash={ingestJobsByHash}
                    deletingTaskId={deletingTaskId}
                    replacingTaskId={replacingTaskId}
                    onDelete={setPendingDeleteTask}
                    onReplace={(task) => void replaceStalledTask(task)}
                  />
                </TaskTimelineItem>
              ))}
              {standaloneActiveJobs.map((job) => (
                <TaskTimelineItem key={job.id} tone={jobTimelineTone(job)}>
                  <ActiveJobFeedItem
                    job={job}
                    cancelling={cancellingJobId === job.id}
                    onCancel={() => void cancelBackgroundJob(job)}
                  />
                </TaskTimelineItem>
              ))}
            </TaskTimelineSection>
          )}

          {showActive && boostTasks.length > 0 && <BoostTaskSection tasks={boostTasks} />}

          {showHistory && standaloneHistoricalJobs.length > 0 && (
            <TaskHistorySection
              jobs={standaloneHistoricalJobs}
              initiallyOpen={view === "history"}
              separated={hasContentBeforeHistory}
              retryingJobId={retryingJobId}
              onRetry={(job) => void retryHistoricalJob(job)}
              undismissingJobId={undismissingJobId}
              onUndismiss={(job) => void restoreDismissedJob(job)}
            />
          )}

          {visibleCount === 0 && !loading && <EmptyView view={view} />}

          {loading && visibleCount === 0 && (
            <div className="flex items-center justify-center gap-2.5 py-20 text-ui text-[var(--text-muted)]">
              <BrandLoader className="size-5" />
              正在汇总任务…
            </div>
          )}

          {pendingDeleteTask && (
            <DeleteDownloadTaskDialog
              task={pendingDeleteTask}
              busy={deletingTaskId === pendingDeleteTask.id}
              onClose={() => {
                if (deletingTaskId == null) setPendingDeleteTask(null);
              }}
              onConfirm={(deleteFiles) => void removeDownloadTask(pendingDeleteTask, deleteFiles)}
            />
          )}
        </>
      )}
    </>
  );
}
