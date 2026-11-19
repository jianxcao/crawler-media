"use client";

import { useEffect, useRef, useState } from "react";

import { JobCard } from "@/components/job-center";
import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import {
  deleteDownloadTask,
  replaceDownloadTask,
  type DownloadTask,
} from "@/lib/api/downloaders";
import { useDownloadTasks } from "@/lib/download-tasks";
import { cancelJob, listJobs, runJob, type JobView } from "@/lib/api/jobs";
import { markJobDismissed, markJobUndismissed } from "@/lib/job-attention";
import { useJobs } from "@/lib/jobs";
import type { TaskCenterViewName } from "@/lib/task-center";
import { useTaskActivity } from "@/lib/task-activity";
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
  { id: "history", label: "已结束" },
];

/**
 * 任务视角：统一“观察入口”，不制造统一状态表。
 * Job 状态来自 crawler-media 数据库，下载状态来自下载器实时快照，订阅关系仅按
 * infohash 投影视图；各自的取消、重试和入库生命周期仍由原领域负责。
 *
 * 页头与一级视角切换由 ActivityView 承担，本组件只渲染状态 tab 与任务内容。
 */
export function TaskCenterView({
  view,
  onViewChange,
}: {
  view: TaskCenterViewName;
  onViewChange: (view: TaskCenterViewName) => void;
}) {
  const setView = onViewChange;
  // 选项卡在窄屏横向滚动：深链直达靠后的视图（如媒体库）时，激活项可能整个
  // 落在可视区之外，用户会以为该视图不存在。挂载与切换时把它滚进来。
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
  const {
    loading,
    error,
    refreshedAt,
    refresh,
  } = useDownloadTasks();

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

  // 页面按“是否需要用户行动”组织，而不是按底层执行器分类。归类与计数由
  // useTaskActivity 统一承担——侧栏角标、一级切换器和这里的状态选项卡必须
  // 用同一份口径，否则同一屏上会出现互相矛盾的数字（见 lib/task-activity）。
  // 每个分区内部仍沿用 Provider 的更新时间倒序，保留最重要的任务在前。
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
  } = useTaskActivity();
  const showAttention = view === "all" || view === "attention";
  const showActive = view === "all" || view === "active";
  const showHistory = view === "all" || view === "history";
  const hasContentBeforeHistory =
    (showAttention && attentionTotal > 0) || (showActive && activeTotal > 0);
  const visibleCount =
    (showAttention ? attentionTotal : 0) +
    (showActive ? activeTotal : 0) +
    (showActive && boostTasks.length > 0 ? 1 : 0) +
    (showHistory ? standaloneHistoricalJobs.length : 0);
  const viewCounts: Partial<Record<TaskCenterViewName, number>> = {
    attention: attentionTotal,
    active: activeTotal,
    history: standaloneHistoricalJobs.length,
  };

  return (
    <>
        {error && <SourceWarning error={error} />}

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
                {item.id !== "all" && (viewCounts[item.id] ?? 0) > 0 && (
                  <span className="tnum ml-1.5 text-caption text-white/45">
                    {viewCounts[item.id]}
                  </span>
                )}
              </button>
            ))}
          </div>
          <p className="shrink-0 pb-2 text-caption text-[var(--text-faint)] max-md:hidden">
            {refreshedAt
              ? `${formatRelativeTime(new Date(refreshedAt).toISOString())}更新`
              : loading
                ? "正在读取下载器"
                : "等待刷新"}
          </p>
        </div>

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

        {showActive && activeTotal > 0 && (
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
              <TaskTimelineItem
                key={job.id}
                tone={jobTimelineTone(job)}
              >
                <ActiveJobFeedItem
                  job={job}
                  cancelling={cancellingJobId === job.id}
                  onCancel={() => void cancelBackgroundJob(job)}
                />
              </TaskTimelineItem>
            ))}
          </TaskTimelineSection>
        )}

        {/* 刷流做种分组：默认折叠，头部常显实时汇总（速度 / 已上传 / 已下载）。
            这些种子没有入库流转语义，不进时间线，也不参与关注判定 */}
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
  );
}
