"use client";

import { useEffect, useMemo, useRef, useState } from "react";

import { Link } from "react-router-dom";

import {
  JOB_STATUS_LABELS,
  JOB_TYPE_LABELS,
  JobCard,
  TaskActionsMenu,
  TaskStatusDot,
} from "@/components/job-center";
import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import {
  ChevronRightIcon,
  CheckIcon,
  FilmIcon,
  InfoIcon,
  TvIcon,
  XIcon,
} from "@/components/icons";
import { Modal } from "@/components/modal";
import { OverflowText } from "@/components/overflow-text";
import {
  deleteDownloadTask,
  replaceDownloadTask,
  type DownloadTask,
} from "@/lib/api/downloaders";
import { useDownloadTasks } from "@/lib/download-tasks";
import { formatBytes } from "@/lib/format";
import {
  buildIngestHistoryDetail,
  type IngestHistoryDetail,
} from "@/lib/ingest-history";
import { shouldOfferInlineReplacement } from "@/lib/download-task-actions";
import {
  cancelJob,
  listJobs,
  runJob,
  isSystemCancelled,
  type JobStatus,
  type JobView,
} from "@/lib/api/jobs";
import {
  isDismissed,
  markJobDismissed,
  markJobUndismissed,
  jobNeedsAttention,
} from "@/lib/job-attention";
import { useJobs } from "@/lib/jobs";
import type { TaskCenterViewName } from "@/lib/task-center";
import {
  downloadTaskNeedsAttention,
  isSubscriptionTask,
  taskSourceOf,
  useTaskActivity,
  type DownloadTaskGroup,
} from "@/lib/task-activity";
import {
  formatClockTime,
  formatNextRunTime,
  formatRelativeTime,
  formatTimelineDayLabel,
  timelineDayKey,
} from "@/lib/time";

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

/** 删除任务的二次确认：安全默认只移除任务，是否删除数据文件由用户显式选择。 */
function DeleteDownloadTaskDialog({
  task,
  busy,
  onClose,
  onConfirm,
}: {
  task: DownloadTask;
  busy: boolean;
  onClose: () => void;
  onConfirm: (deleteFiles: boolean) => void;
}) {
  const [deleteFiles, setDeleteFiles] = useState(false);
  const downloaderName = task.downloader_name || `下载器 ${task.downloader_id}`;

  return (
    <Modal open onClose={busy ? () => {} : onClose} label="删除种子任务" topmost>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-white">删除种子任务？</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          将从「{downloaderName}」停止并移除该任务。
        </p>
        <p className="mt-3 break-words rounded-xl border border-white/[0.08] bg-white/[0.035] px-3.5 py-3 text-sub leading-6 text-white/75">
          {task.name || task.info_hash}
        </p>
        <label
          className={`mt-4 flex cursor-pointer items-start gap-3 rounded-xl border px-3.5 py-3 transition ${
            deleteFiles
              ? "border-[#ff6b6b]/35 bg-[#ff6b6b]/[0.08]"
              : "border-white/[0.08] bg-white/[0.025] hover:bg-white/[0.045]"
          }`}
        >
          <input
            type="checkbox"
            checked={deleteFiles}
            onChange={(event) => setDeleteFiles(event.target.checked)}
            className="mt-1 size-4 shrink-0 accent-[#ff6b6b]"
          />
          <span className="min-w-0">
            <span className="block text-ui font-semibold text-white/90">同时删除数据文件</span>
            <span className="mt-0.5 block text-caption leading-5 text-white/50">
              包括已下载和未完成的数据；若文件仍由下载器管理，即使已经入库也可能被删除，且无法恢复。
            </span>
          </span>
        </label>
        {task.subscriptions.length > 0 && (
          <p className="mt-3 text-caption leading-5 text-amber-100/65">
            关联订阅会保留原工单身份；巡检确认任务无进度后会按换源策略寻找替代源。
          </p>
        )}
        <div className="mt-5 flex justify-end gap-2.5">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded-lg border border-white/10 bg-white/[0.06] px-4 py-2 text-ui text-white/80 transition hover:bg-white/[0.1] disabled:opacity-40"
          >
            取消
          </button>
          <button
            type="button"
            onClick={() => onConfirm(deleteFiles)}
            disabled={busy}
            className="rounded-lg bg-red-500/85 px-4 py-2 text-ui font-medium text-white transition hover:bg-red-500 disabled:opacity-40"
          >
            {busy ? "正在删除…" : deleteFiles ? "删除任务和文件" : "仅删除任务"}
          </button>
        </div>
      </div>
    </Modal>
  );
}

function SourceWarning({ error }: { error: string }) {
  return (
    <div className="mt-4 flex items-start gap-3 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub leading-6 text-amber-100">
      <InfoIcon className="mt-0.5 size-4 shrink-0" />
      <p className="min-w-0 flex-1">{error}</p>
      <Link to={"/settings/downloaders"}
        className="shrink-0 font-semibold text-amber-50 hover:underline"
      >
        检查设置
      </Link>
    </div>
  );
}

/** 需要用户行动的任务脱离普通时间流置顶，避免失败记录被持续刷新的进度淹没。 */
function TaskAttentionSection({
  count,
  canDismissAll,
  dismissingAll,
  onDismissAll,
  children,
}: {
  count: number;
  /** 有没有"已经失败、还没被忽略"的任务；没有就不摆一个点了没反应的按钮。 */
  canDismissAll: boolean;
  dismissingAll: boolean;
  onDismissAll: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-6" aria-labelledby="attention-tasks-title">
      <div className="mb-3 flex items-center gap-2.5">
        <span className="size-2 rounded-full bg-[var(--danger)] ring-4 ring-[var(--danger)]/10" />
        <h2 id="attention-tasks-title" className="text-ui font-semibold text-[var(--danger)]">
          需要你处理
        </h2>
        <span className="tnum rounded-full bg-[var(--danger)]/10 px-2 py-0.5 text-caption text-[var(--danger)]">
          {count}
        </span>
        {/* 故障常常成批：一次扫描收尾可能建出几十个字幕任务，配置错了就几十条
            一起失败，逐条点忽略是灾难。整体动作压成次要文字按钮，不跟每张卡
            自己的「重试」抢视觉。 */}
        {canDismissAll && (
          <button
            type="button"
            onClick={onDismissAll}
            disabled={dismissingAll}
            className="ml-auto shrink-0 rounded-lg px-2.5 py-1 text-caption font-medium text-white/40 transition hover:bg-white/[0.05] hover:text-white/70 disabled:cursor-wait disabled:opacity-40"
          >
            {dismissingAll ? "正在忽略…" : "全部忽略"}
          </button>
        )}
      </div>
      <div className="space-y-2.5 rounded-2xl border border-[var(--danger)]/20 bg-[var(--danger)]/[0.035] p-3.5 max-md:p-3">
        {children}
      </div>
    </section>
  );
}

/**
 * 统一时间线只负责排列视图，不合并领域状态。下载卡仍读取下载器快照，JobCard
 * 仍读取 Job；时间轴仅把它们放入同一条用户可扫描的阅读路径。
 */
function TaskTimelineSection({
  title,
  count,
  children,
}: {
  title: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <section className="mt-7" aria-labelledby="active-tasks-title">
      <div className="mb-3 flex items-center gap-3">
        <h2 id="active-tasks-title" className="text-ui font-semibold text-white/65">
          {title}
        </h2>
        <span className="tnum text-caption text-white/30">{count}</span>
        <span aria-hidden="true" className="h-px min-w-8 flex-1 bg-white/[0.09]" />
      </div>
      <div>{children}</div>
    </section>
  );
}

/**
 * 时间轴条目。原本桌面端在最左预留一条 3.75rem 的时刻沟槽，结果整段内容被推离
 * 左边界近 100px，与分组标题、上方的筛选标签全部错开；"现在"区的下载过程更没有
 * 单一时刻，那一列直接是空白。
 *
 * 现在只保留圆点轨道：条目与分组标题左对齐，桌面与移动端缩进一致。需要显示时刻
 * 的条目（历史记录）自己把时间排进内容里，避免为一列时间牺牲整段的对齐。
 */
function TaskTimelineItem({
  tone = "active",
  children,
}: {
  tone?: "active" | "waiting" | "success" | "cancelled";
  children: React.ReactNode;
}) {
  const completed = tone === "success" || tone === "cancelled";
  const dotClass =
    tone === "waiting"
      ? "bg-[var(--warn)] ring-[var(--warn)]/25"
      : tone === "success"
        ? "bg-[var(--ok)] text-[#07120b] ring-[var(--ok)]/20"
        : tone === "cancelled"
          ? "bg-white/20 text-white/60 ring-white/10"
          : "bg-[var(--info)] ring-[var(--info)]/30";
  return (
    <div className="grid grid-cols-[1.5rem_minmax(0,1fr)] gap-x-2 last:[&_[data-timeline-line]]:hidden max-md:grid-cols-[1.25rem_minmax(0,1fr)] max-md:gap-x-2.5">
      <span className="relative flex self-stretch justify-center" aria-hidden="true">
        <span
          className={`absolute top-[0.7rem] z-10 flex items-center justify-center rounded-full border-2 border-[#0b0f17] ring-2 ${
            completed ? "size-4" : "mt-0.5 size-2.5"
          } ${dotClass}`}
        >
          {tone === "success" && <CheckIcon className="size-2.5" />}
          {tone === "cancelled" && <XIcon className="size-2.5" />}
        </span>
        <span
          data-timeline-line
          className="absolute bottom-0 top-[1.65rem] w-px bg-white/[0.12]"
        />
      </span>
      <div className="min-w-0 pb-4">{children}</div>
    </div>
  );
}

function jobTimelineTone(job: JobView): "active" | "waiting" {
  return job.last_status === "running" ? "active" : "waiting";
}

const ACTIVE_JOB_ACTIONS: Record<string, string> = {
  "subtitle.generate": "正在生成字幕",
  "library.scan": "正在扫描",
  "library.metadata.refresh": "正在刷新媒体库元数据",
  "media.metadata.refresh": "正在刷新元数据",
  "library.chapter_images": "正在生成章节",
  "library.organize": "正在整理文件",
  "library.transfer": "正在转移文件",
  "library.ingest": "正在入库",
  subscribe_search: "正在搜索资源",
  subscribe_rss: "正在抓取 RSS",
  transfer: "正在转移文件",
  scrape: "正在刮削元数据",
  check_in: "正在打卡签到",
  catalog_refresh: "正在刷新元数据",
  watch_intake: "正在导入监控目录",
};

const COMPLETED_JOB_ACTIONS: Record<string, string> = {
  "subtitle.generate": "字幕生成",
  "library.scan": "扫描",
  "library.metadata.refresh": "元数据刷新",
  "media.metadata.refresh": "元数据刷新",
  "library.chapter_images": "章节生成",
  "library.organize": "文件整理",
  "library.transfer": "文件转移",
  "library.ingest": "入库",
  subscribe_search: "资源搜索",
  subscribe_rss: "RSS 抓取",
  transfer: "文件转移",
  scrape: "元数据刮削",
  check_in: "打卡签到",
  catalog_refresh: "元数据刷新",
  watch_intake: "目录导入",
};

function jobDetailString(job: JobView, key: string): string | null {
  // 新契约 JobDef 无运行期 progress，只能读入参 payload；运行期的 message 等
  // 键名若入参里有（如 file_name/media_title）仍能取到。
  const detail = job.payload[key];
  return typeof detail === "string" && detail ? detail : null;
}

function jobDetailNumber(job: JobView, key: string): number | null {
  const detail = job.payload[key];
  return typeof detail === "number" && Number.isFinite(detail) ? detail : null;
}

/** Feed 主标题优先使用执行结果确认过的作品名，避免把长 release 名当业务标题。 */
function jobFeedIdentity(job: JobView): string | null {
  if (job.friendly_name) {
    return job.friendly_name;
  }
  const title =
    jobDetailString(job, "media_title") ||
    jobDetailString(job, "title") ||
    job.name;
  if (!title) return null;
  if (/^[《「].+[》」]$/.test(title)) return title;
  if (job.kind === "library.scan") return `「${title}」`;
  if (
    job.kind === "library.ingest" ||
    job.kind === "subtitle.generate" ||
    job.kind.includes("metadata.refresh")
  ) {
    return `《${title}》`;
  }
  return title;
}

function jobOriginLabel(job: JobView): string {
  // 新契约 JobDef 无 origin/actor_name；有调度定义（schedule）的任务视为系统自动。
  return job.schedule ? "系统自动" : "后台任务";
}

function activeJobStatus(job: JobView): string {
  if (job.last_status === "running") {
    return ACTIVE_JOB_ACTIONS[job.kind] || "正在处理";
  }
  return JOB_STATUS_LABELS[job.last_status ?? ""] || job.last_status || "等待执行";
}

function historicalJobTitle(job: JobView): string {
  const identity = jobFeedIdentity(job);
  const action = COMPLETED_JOB_ACTIONS[job.kind];
  // 忽略只是"不再提醒"，不是"做完了"。历史里如实写「未完成」，否则用户回头
  // 翻记录会以为这件事已经成了。
  const result =
    job.last_status === "cancelled" ? "已取消" : job.last_status === "failed" ? "未完成" : "完成";
  if (identity && action) return `${identity}${action}${result}`;
  return `${identity || JOB_TYPE_LABELS[job.kind] || job.kind}${result}`;
}

function historicalJobSummary(job: JobView): string {
  if (job.kind === "library.scan") {
    const identified = jobDetailNumber(job, "identified");
    const unidentified = jobDetailNumber(job, "unidentified");
    const parts = [
      identified != null ? `识别 ${identified} 个` : null,
      unidentified != null ? `待识别 ${unidentified} 个` : null,
    ].filter((item): item is string => item != null);
    if (parts.length > 0) return parts.join(" · ");
  }
  if (job.last_status === "failed" && isDismissed(job)) {
    return job.last_error || jobDetailString(job, "message") || "任务失败，已被忽略";
  }
  return (
    (job.last_status === "failed" ? job.last_error : null) ||
    jobDetailString(job, "message") ||
    (job.last_status === "cancelled" ? "任务已取消" : "任务已完成")
  );
}

function ActiveJobFeedItem({
  job,
  cancelling,
  onCancel,
}: {
  job: JobView;
  cancelling: boolean;
  onCancel: () => void;
}) {
  const cancellable = job.last_status !== "cancelled";
  const title = jobFeedIdentity(job) || JOB_TYPE_LABELS[job.kind] || job.kind;
  return (
    <article className="min-w-0 py-1.5">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-ui font-semibold leading-5 text-white/90">
            <OverflowText>{title}</OverflowText>
          </h3>
          <p className="mt-1 flex flex-wrap items-center gap-x-1.5 text-sub text-white/60">
            <span className="font-medium text-white/70">{activeJobStatus(job)}</span>
          </p>
        </div>
        {cancellable && (
          <button
            type="button"
            onClick={onCancel}
            disabled={cancelling}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/45 transition hover:bg-white/[0.06] hover:text-white/75 disabled:cursor-wait disabled:opacity-45"
          >
            {cancelling ? "正在取消…" : "取消任务"}
          </button>
        )}
      </div>
      {jobDetailString(job, "message") && (
        <OverflowText lines={2} className="mt-1.5 text-caption leading-5 text-white/42">
          {jobDetailString(job, "message")}
        </OverflowText>
      )}
      <p className="mt-2 text-caption text-white/30">
        {jobOriginLabel(job)}
        {job.last_status === "queued" &&
          job.next_run_after != null &&
          job.next_run_after !== "" && (
          <>
            <span aria-hidden="true"> · </span>
            {job.next_run_after === 0
              ? "即将执行"
              : `${formatNextRunTime(job.next_run_after)} 下次运行`}
          </>
        )}
        {job.last_status === "running" && (
          <>
            <span aria-hidden="true"> · </span>
            正在执行
          </>
        )}
      </p>
    </article>
  );
}

interface HistoryDayGroup {
  key: string;
  label: string;
  jobs: JobView[];
}

function groupHistoricalJobs(jobs: JobView[]): HistoryDayGroup[] {
  const groups = new Map<string, HistoryDayGroup>();
  for (const job of jobs) {
    const timestamp = job.last_finished_at || job.next_run_after || "";
    const key = timelineDayKey(timestamp);
    const existing = groups.get(key);
    if (existing) {
      existing.jobs.push(job);
    } else {
      groups.set(key, { key, label: formatTimelineDayLabel(timestamp), jobs: [job] });
    }
  }
  return [...groups.values()];
}

function HistoricalJobFeedItem({
  job,
  time,
  retrying,
  onRetry,
  undismissing,
  onUndismiss,
}: {
  job: JobView;
  /** 完成时刻。不再占用左侧独立列，跟摘要同行显示，让条目与分组标题左对齐。 */
  time: string;
  retrying: boolean;
  onRetry: () => void;
  undismissing: boolean;
  onUndismiss: () => void;
}) {
  // 忽略必须能反悔——用户是在信息不全时按下的它（"先不管这个"），
  // 撤销入口就得留在他能找回这条记录的地方，而不是只能靠重试再造一条新任务。
  const dismissed = job.last_status === "failed" && isDismissed(job);
  const ingestDetail = buildIngestHistoryDetail(job);
  return (
    <article className="group/history min-w-0 py-1.5">
      <div className="flex min-w-0 items-start gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-ui font-medium leading-5 text-white/78">
            <OverflowText>{historicalJobTitle(job)}</OverflowText>
          </h3>
          <div className="mt-1 flex min-w-0 items-baseline gap-2 text-caption leading-5 text-white/40">
            <span className="tnum shrink-0 text-white/32">{time}</span>
            <OverflowText lines={2} className="min-w-0 flex-1">
              {ingestDetail?.summary ?? historicalJobSummary(job)}
            </OverflowText>
          </div>
          {ingestDetail && <IngestHistoryFiles detail={ingestDetail} />}
        </div>
        {dismissed && (
          <button
            type="button"
            onClick={onUndismiss}
            disabled={undismissing}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/35 transition hover:bg-white/[0.05] hover:text-white/65 disabled:cursor-wait disabled:opacity-40"
          >
            {undismissing ? "撤销中…" : "撤销忽略"}
          </button>
        )}
        {job.last_status === "cancelled" && !isSystemCancelled(job) && (
          <button
            type="button"
            onClick={onRetry}
            disabled={retrying}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/35 transition hover:bg-white/[0.05] hover:text-white/65 disabled:cursor-wait disabled:opacity-40"
          >
            {retrying ? "重新执行中…" : "重新执行"}
          </button>
        )}
      </div>
    </article>
  );
}

/** 入库历史默认只占一行，需要核对时再展开真实文件名与目标媒体库。 */
function IngestHistoryFiles({ detail }: { detail: IngestHistoryDetail }) {
  if (detail.fileGroups.length === 0 && detail.context == null) return null;
  const fileCount = detail.fileGroups.reduce((total, group) => total + group.files.length, 0);
  return (
    <details className="group/files mt-1.5 max-w-full text-caption">
      <summary className="flex w-fit max-w-full cursor-pointer list-none items-center gap-1.5 rounded-md py-1 pr-1 text-white/32 transition hover:text-white/60 [&::-webkit-details-marker]:hidden">
        <ChevronRightIcon className="size-3.5 shrink-0 transition-transform group-open/files:rotate-90" />
        <span>查看文件明细</span>
        {fileCount > 0 && <span className="tnum text-white/22">{fileCount} 个</span>}
      </summary>
      <div className="mt-1.5 hidden rounded-lg border border-white/[0.06] bg-white/[0.025] px-3 py-2.5 group-open/files:block">
        {detail.context && <p className="mb-2 text-white/38">{detail.context}</p>}
        <div className="space-y-2.5">
          {detail.fileGroups.map((group) => (
            <section key={group.label} aria-label={group.label}>
              <p className="mb-1 text-micro font-medium text-white/35">
                {group.label} · {group.files.length} 个
              </p>
              <ul className="space-y-1 text-white/52">
                {group.files.map((file) => (
                  <li key={file} title={file} className="break-all leading-5">
                    {file}
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </div>
    </details>
  );
}

/** 历史按本地日期拆成轻量 Feed，首组默认展开，避免完成卡片占满纵向空间。 */
/**
 * 实时速度的统一配色：上传走 --ok 绿（做种的"战果"），下载走 --info 蓝（"正在进行"），
 * 数值加粗从灰色标签里跳出来。所有来自下载器（qB/Tr）的任务——刷流汇总、刷流单行、
 * 普通任务的实时行——都走这一个组件，保证任务中心里 ↑/↓ 的颜色语义处处一致。
 * glow 只给刷流汇总头部用（折叠时也要一眼可见）；speed 为 0/空时给破折号或灰字占位，
 * 由调用方决定是否渲染占位（固定列需要占位防塌陷，自由流式行则直接不渲染）。
 */
function SpeedStat({
  direction,
  bytesPerSecond,
  glow = false,
  placeholder,
  className,
}: {
  direction: "up" | "down";
  bytesPerSecond: number | null | undefined;
  glow?: boolean;
  /** 速度为空/0 时的占位文案；不传则渲染 null */
  placeholder?: string;
  className?: string;
}) {
  const active = bytesPerSecond != null && bytesPerSecond > 0;
  if (!active) {
    return placeholder != null ? (
      <span className={`tnum text-white/20 ${className ?? ""}`}>{placeholder}</span>
    ) : null;
  }
  const tone =
    direction === "up"
      ? `text-[var(--ok)] ${glow ? "drop-shadow-[0_0_6px_rgba(74,222,128,0.45)]" : ""}`
      : `text-[var(--info)] ${glow ? "drop-shadow-[0_0_6px_rgba(127,176,255,0.45)]" : ""}`;
  return (
    <span className={`tnum font-semibold ${tone} ${className ?? ""}`}>
      {direction === "up" ? "↑" : "↓"} {formatBytes(bytesPerSecond)}/s
    </span>
  );
}

/**
 * 刷流做种分组：默认折叠的 <details>，头部常显最值得关心的实时汇总——
 * ↑/↓ 总速度与已上传/已下载总量；展开后逐种子一行（站点 + 名称 + 状态 +
 * 各自的速度与累计上传）。刷流种子没有媒体身份与入库流转，刻意不渲染
 * 生命周期，也不提供删除入口（汰换归引擎管，手动删除去下载器按
 * boost 分类操作）。
 */
function BoostTaskSection({ tasks }: { tasks: DownloadTask[] }) {
  const sum = (pick: (task: DownloadTask) => number | null) =>
    tasks.reduce((total, task) => total + (pick(task) ?? 0), 0);
  const upSpeed = sum((t) => t.upspeed_bytes);
  const downSpeed = sum((t) => t.dlspeed_bytes);
  const uploaded = sum((t) => t.uploaded_bytes);
  const downloaded = sum((t) => t.completed_bytes);
  // 刷流列表最想回答的是"现在哪些种子在出力"，所以按上行速度倒序，正在上传的
  // 永远浮在最前；速度相同（绝大多数是 0）时用累计上传量兜底，让贡献大的靠前，
  // 也让静默种子之间的顺序在刷新之间保持稳定。
  const sorted = useMemo(
    () =>
      [...tasks].sort(
        (a, b) =>
          (b.upspeed_bytes ?? 0) - (a.upspeed_bytes ?? 0) ||
          (b.uploaded_bytes ?? 0) - (a.uploaded_bytes ?? 0),
      ),
    [tasks],
  );

  return (
    <section className="mt-3" aria-label="刷流做种">
      <details className="group border-b border-white/[0.07] py-3.5 last:border-b-0">
        <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 rounded-lg py-1 text-ui transition [&::-webkit-details-marker]:hidden">
          <span className="font-semibold text-[var(--accent)]">刷流做种</span>
          <span className="tnum text-caption text-white/60">{tasks.length} 个种子</span>
          {/* 实时汇总：速度是"现在"，总量是"战果"——都放头部，折叠时也一眼可见。
              上传走 --ok 绿（刷流的战果就是上传量），下载走 --info 蓝，数值带淡辉光
              从灰色标签里跳出来；标签本身保持浅灰，让数字成为视觉焦点。
              窄屏这四项塞不进标题行，整块换到第二行（order-2 + w-full）独占一行，
              避免和"展开"挤在一起互相把对方顶出去。第二行再排成 2×2 网格：速度一行、
              总量一行——四项自由换行时"已下载"会随速度值的宽窄时而落单成第三行，
              网格让分组头部的高度和对齐不随数字长短抖动。 */}
          <span className="tnum flex flex-wrap items-center gap-x-3 text-caption text-white/45 max-md:order-2 max-md:grid max-md:w-full max-md:grid-cols-2 max-md:gap-y-1">
            {/* 上下行速度常显：为 0 时退成灰色占位，让"现在没在下载"也是一条信息 */}
            <SpeedStat direction="up" bytesPerSecond={upSpeed} glow placeholder="↑ 0 B/s" />
            <SpeedStat direction="down" bytesPerSecond={downSpeed} glow placeholder="↓ 0 B/s" />
            <span>
              已上传 <span className="font-semibold text-[var(--ok)]">{formatBytes(uploaded)}</span>
            </span>
            <span>
              已下载 <span className="font-semibold text-[var(--info)]">{formatBytes(downloaded)}</span>
            </span>
          </span>
          {/* 展开态文案和箭头必须同进同退，所以包在一个不可换行的组里：上一版两者是
              summary 的兄弟节点，窄屏换行时箭头会被单独甩到下一行成为孤儿。
              窄屏 order-1 让它跟在"N 个种子"后面停在标题行右端。 */}
          <span className="ml-auto flex shrink-0 items-center gap-1 whitespace-nowrap text-caption text-white/30 max-md:order-1">
            <span className="group-open:hidden">展开</span>
            <span className="hidden group-open:inline">收起</span>
            <ChevronRightIcon className="size-4 transition-transform group-open:rotate-90" />
          </span>
        </summary>
        <div className="mt-2 divide-y divide-white/[0.05]">
          {sorted.map((task) => (
            <BoostTaskRow key={task.id} task={task} />
          ))}
        </div>
      </details>
    </section>
  );
}

/**
 * 刷流单行。数字列走固定宽度而不是跟着内容伸缩——上一版把站点、名称、数字全塞进
 * 一个 flex 行，每行数字个数不同就把名称挤成不同长度，180 行下来像没对齐的乱码。
 *
 * 现在的分工：站点与名称占左半区（名称吃掉全部剩余宽度），上行速度 / 累计上传 /
 * 体积三列右对齐且逐行等宽；缺值补破折号，保证列不塌陷。窄屏时数字整块换到第二行，
 * 让名称在小屏也有完整宽度。
 *
 * 下行速度刻意不占固定列：刷流种子绝大多数只做种不下载，给它留一列的结果是整张列表
 * 挂着一排毫无信息量的破折号，还把上行速度挤到要换行。它改成和进度百分比一起跟在
 * 名称后面，只在真的在下载时出现。
 */
function BoostTaskRow({ task }: { task: DownloadTask }) {
  const downloading = task.state === "downloading";
  const percent = task.progress == null ? null : Math.floor(task.progress * 100);
  const showDownloadNote = (downloading && percent != null) || (task.dlspeed_bytes ?? 0) > 0;
  const name = (
    <OverflowText className="min-w-0 flex-1 text-ui text-white/80">
      {task.name || task.info_hash}
    </OverflowText>
  );
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 py-2">
      <div className="flex w-[4.5rem] shrink-0">
      </div>
      {name}
      {/* 窄屏字号比桌面大一档（--text-caption 11px → 13px），5.5rem 装不下
          "↑ 63.95 KB/s"，速度会被折成两行。窄屏改用按内容需求配比的弹性列
          （速度 : 累计 : 体积 ≈ 9 : 10 : 7），列宽随屏宽缩放而比例不变——既保住逐行
          对齐，也让 360px 的小屏和 430px 的大屏都留有余量；nowrap 再兜一道底 */}
      <div className="tnum grid shrink-0 grid-cols-[5.5rem_7rem_5rem] items-center gap-x-3 whitespace-nowrap text-right text-caption text-white/40 max-md:w-full max-md:grid-cols-[9fr_10fr_7fr] max-md:gap-x-2">
        <SpeedStat direction="up" bytesPerSecond={task.upspeed_bytes} placeholder="—" />
        <span>
          {task.uploaded_bytes != null && task.uploaded_bytes > 0
            ? `累计 ↑${formatBytes(task.uploaded_bytes)}`
            : "—"}
        </span>
        <span>{task.size_bytes != null ? formatBytes(task.size_bytes) : "—"}</span>
      </div>
      {/* 下载中的刷流种子是少数，进度与下行速度不占固定列，另起一行挂在数字列下方：
          既不因为多出两个值把逐行对齐搞乱，也保证 ↑ 永远排在 ↓ 前面，和分组汇总
          头部（↑ 速度 → ↓ 速度 → 已上传 → 已下载）的"上传在前"口径一致 */}
      {showDownloadNote && (
        <div className="flex w-full items-center justify-end gap-x-2 text-caption text-white/35">
          {downloading && percent != null && <span className="tnum">{percent}%</span>}
          <SpeedStat direction="down" bytesPerSecond={task.dlspeed_bytes} />
        </div>
      )}
    </div>
  );
}

function TaskHistorySection({
  jobs,
  initiallyOpen,
  separated,
  retryingJobId,
  onRetry,
  undismissingJobId,
  onUndismiss,
}: {
  jobs: JobView[];
  initiallyOpen: boolean;
  separated: boolean;
  retryingJobId: string | null;
  onRetry: (job: JobView) => void;
  undismissingJobId: string | null;
  onUndismiss: (job: JobView) => void;
}) {
  const groups = groupHistoricalJobs(jobs);
  return (
    <section
      className={separated ? "mt-6 border-t border-white/[0.1]" : "mt-3"}
      aria-label="历史任务"
    >
      {groups.map((group, index) => (
        <details
          key={group.key}
          open={initiallyOpen || index === 0 || undefined}
          className="group border-b border-white/[0.07] py-3.5 last:border-b-0"
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg py-1 text-ui text-white/55 transition hover:text-white/80 [&::-webkit-details-marker]:hidden">
            <span className="font-semibold">{group.label}</span>
            <span className="tnum text-caption text-white/30">{group.jobs.length} 项</span>
            <span className="ml-auto text-caption text-white/30 group-open:hidden">展开</span>
            <span className="ml-auto hidden text-caption text-white/30 group-open:inline">收起</span>
            <ChevronRightIcon className="size-4 transition-transform group-open:rotate-90" />
          </summary>
          <div className="mt-3">
            {group.jobs.map((job) => (
              <TaskTimelineItem
                key={job.id}
                tone={job.last_status === "succeeded" ? "success" : "cancelled"}
              >
                <HistoricalJobFeedItem
                  job={job}
                  time={formatClockTime(job.last_finished_at ?? job.next_run_after)}
                  retrying={retryingJobId === job.id}
                  onRetry={() => onRetry(job)}
                  undismissing={undismissingJobId === job.id}
                  onUndismiss={() => onUndismiss(job)}
                />
              </TaskTimelineItem>
            ))}
          </div>
        </details>
      ))}
    </section>
  );
}

/**
 * 一个任务状态的展示三件套。``color`` 走内联样式（进度条与状态文字共用），
 * ``dot`` 是状态点的 class。
 *
 * 取色只从 globals.css 的状态四档里挑（--ok / --info / --warn / --danger），
 * 「没有 / 不会发生」不占状态色，用本表既有的中性灰。判断口径见 globals.css：
 * 颜色说的是「系统状态」还是「你该做什么」，只有后者用 --danger。
 *
 * 同一档里的多个状态长得一样是刻意的——「下载中」和「校验中」都是正在进行，
 * 区别由 label 承担；用色相去暗示它们是两类不同的东西，只会稀释颜色的信号。
 */
interface TaskStateMeta {
  label: string;
  color: string;
  dot: string;
}

const DOWNLOAD_STATE_META: Record<DownloadTask["state"], TaskStateMeta> = {
  downloading: {
    label: "下载中",
    color: "var(--info)",
    dot: "animate-pulse bg-[var(--info)]",
  },
  stalled: {
    label: "等待连接",
    color: "var(--warn)",
    dot: "bg-[var(--warn)]",
  },
  paused: {
    label: "已暂停",
    color: "var(--warn)",
    dot: "bg-[var(--warn)]",
  },
  queued: {
    label: "排队中",
    color: "#cbd5e1",
    dot: "bg-white/40",
  },
  checking: {
    label: "校验中",
    color: "var(--info)",
    dot: "animate-pulse bg-[var(--info)]",
  },
  completed: {
    label: "等待入库",
    color: "var(--ok)",
    dot: "bg-[var(--ok)]",
  },
  error: {
    label: "下载异常",
    color: "var(--danger)",
    dot: "bg-[var(--danger)]",
  },
  missing: {
    label: "任务缺失",
    color: "var(--danger)",
    dot: "bg-[var(--danger)]",
  },
  unknown: {
    label: "状态未知",
    color: "#cbd5e1",
    dot: "bg-white/40",
  },
};

const INGEST_STATE_META: Record<JobStatus, TaskStateMeta> = {
  queued: { label: "等待入库", color: "#cbd5e1", dot: "bg-white/40" },
  running: { label: "正在入库", color: "var(--ok)", dot: "animate-pulse bg-[var(--ok)]" },
  succeeded: { label: "入库完成", color: "var(--ok)", dot: "bg-[var(--ok)]" },
  failed: { label: "入库待处理", color: "var(--danger)", dot: "bg-[var(--danger)]" },
  // 已取消是终态，不会再有进展：归到本表的中性灰，和「等待入库」一样不占
  // 状态色——它们的区别本来就该由 label 说清楚。
  cancelled: { label: "入库已取消", color: "#cbd5e1", dot: "bg-white/40" },
};

/**
 * 一个下载任务有两条独立进度轴：下载轴的事实源永远是下载器，入库轴的事实源
 * 是逐集工单状态。入库 Job 只是「此刻正在搬」的瞬时装饰——边下边入库时，两
 * 批搬运之间没有任何 Job 存活，用「有没有 Job」当状态源会把「整季只下了
 * 19%、其中先入库 1 集」显示成「下载完成 + 入库完成 100%」（真实事故）。
 *
 * 因此只有两种情况允许入库轴接管卡片主状态：整包已下完（入库确实是此刻唯一
 * 在推进的事），或入库受阻需要人处理（必须顶到最前，不能被下载进度盖住）。
 */
function ingestOwnsTaskState(task: DownloadTask, ingestJob: JobView | null): boolean {
  if (ingestJob == null || ingestJob.last_status === "cancelled") return false;
  // 用户已经忽略的失败入库不再顶到最前——他要的正是"别再拿这件事挡着我"，
  // 卡片交回下载轴如实讲进度。
  if (jobNeedsAttention(ingestJob)) return true;
  return task.state === "completed";
}

function downloadGroupTimelineTone(
  group: DownloadTaskGroup,
  ingestJobsByHash: Map<string, JobView>,
): "active" | "waiting" {
  const executing = group.tasks.some((task) => {
    const ingestJob = task.info_hash ? ingestJobsByHash.get(task.info_hash.toLowerCase()) : undefined;
    return ingestJob?.last_status === "running" || ["downloading", "checking"].includes(task.state);
  });
  return executing ? "active" : "waiting";
}

/** “现在”区域使用过程行，不再把下载卡片原样塞进时间轴。 */
function DownloadTaskGroupFeed({
  group,
  ingestJobsByHash,
  deletingTaskId,
  replacingTaskId,
  onDelete,
  onReplace,
}: {
  group: DownloadTaskGroup;
  ingestJobsByHash: Map<string, JobView>;
  deletingTaskId: string | null;
  replacingTaskId: string | null;
  onDelete: (task: DownloadTask) => void;
  onReplace: (task: DownloadTask) => void;
}) {
  const grouped = group.mediaItemId != null;
  const firstSubscription = group.tasks.flatMap((task) => task.subscriptions)[0];
  return (
    <article className="min-w-0 py-1.5">
      {grouped && (
        <div className="mb-2 flex min-w-0 items-start justify-between gap-3">
          <div className="min-w-0 flex-1">
            <h3 className="flex min-w-0 items-center gap-2 text-ui font-semibold leading-5 text-white/90">
              <span className="shrink-0 rounded-full border border-[var(--info)]/25 bg-[var(--info)]/[0.12] px-1.5 py-0.5 text-[11px] font-semibold leading-none text-[var(--info)]">
                实时
              </span>
              <OverflowText className="flex-1">{group.title}</OverflowText>
            </h3>
            <p className="mt-1 text-caption text-white/35">
              {group.kind === "tv" ? "剧集" : "电影"}
              <span aria-hidden="true"> · </span>
              {group.tasks.length} 个下载资源
            </p>
          </div>
          {firstSubscription && (
            <Link to={`/subscriptions/${firstSubscription.id}`}
              className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/40 transition hover:bg-white/[0.06] hover:text-white/70"
            >
              查看订阅
            </Link>
          )}
        </div>
      )}
      <div
        className={
          grouped
            ? "divide-y divide-white/[0.06] border-l border-white/[0.1] pl-3.5"
            : ""
        }
      >
        {group.tasks.map((task) => (
          <DownloadTaskFeedItem
            key={task.id}
            task={task}
            ingestJob={task.info_hash ? (ingestJobsByHash.get(task.info_hash.toLowerCase()) ?? null) : null}
            grouped={grouped}
            deleting={deletingTaskId === task.id}
            replacing={replacingTaskId === task.id}
            onDelete={onDelete}
            onReplace={onReplace}
          />
        ))}
      </div>
    </article>
  );
}

function DownloadTaskFeedItem({
  task,
  ingestJob,
  grouped,
  deleting,
  replacing,
  onDelete,
  onReplace,
}: {
  task: DownloadTask;
  ingestJob: JobView | null;
  grouped: boolean;
  deleting: boolean;
  replacing: boolean;
  onDelete: (task: DownloadTask) => void;
  onReplace: (task: DownloadTask) => void;
}) {
  const ingestOwnsState = ingestOwnsTaskState(task, ingestJob);
  const meta =
    ingestOwnsState && ingestJob && ingestJob.last_status != null
      ? INGEST_STATE_META[ingestJob.last_status]
      : DOWNLOAD_STATE_META[task.state];
  const downloadPercent = task.progress == null ? null : Math.floor(task.progress * 100);
  const percent = downloadPercent;
  const title = grouped
    ? task.name || task.media_title || task.info_hash || task.id
    : task.media_title || task.name || task.info_hash || task.id;
  // 有关联入库 Job 时不再单独出一行文字说明：入库阶段与解释由底部生命
  // 周期承担，覆盖集标签会标注已入库进度，feed 行只保留下载态/换源提示
  const note = ingestJob != null ? null : downloadTaskNote(task, null);
  const showReplace = shouldOfferInlineReplacement(task);
  // 种子名前用站点徽标回答"这个资源从哪来"；画面规格与文件尺寸紧随
  // 其后，弥补高度同质化的 release 名。状态与进度由底部生命周期承担，不再
  // 重复；速度与剩余时间单独一行，专注实时传输信息。
  const specs = [
    task.size_bytes != null ? formatBytes(task.size_bytes) : null,
  ].filter((item): item is string => item != null);
  // 下行速度只在真正下载时有意义；上行（做种）在下载中和等待入库期间都可能存在
  const downSpeed =
    task.state === "downloading" &&
    task.dlspeed_bytes != null &&
    task.dlspeed_bytes > 0
      ? task.dlspeed_bytes
      : null;
  const upSpeed = task.upspeed_bytes != null && task.upspeed_bytes > 0 ? task.upspeed_bytes : null;

  return (
    <div className={grouped ? "py-2.5 first:pt-1 last:pb-1" : ""}>
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h3
            className={`flex min-w-0 items-center gap-2 ${
              grouped ? "text-sub" : "text-ui"
            } font-semibold leading-5 text-white/88`}
          >
            {!grouped && (
              <span className="shrink-0 rounded-full border border-[var(--info)]/25 bg-[var(--info)]/[0.12] px-1.5 py-0.5 text-[11px] font-semibold leading-none text-[var(--info)]">
                实时
              </span>
            )}

            <OverflowText className="flex-1">{title}</OverflowText>
          </h3>
          {specs.length > 0 && (
            <p className="mt-1.5 flex flex-wrap items-center gap-x-1.5 text-caption text-white/88">
              {specs.map((spec, index) => (
                <span key={spec} className="flex items-center gap-x-1.5">
                  {index > 0 && (
                    <span aria-hidden="true" className="text-white/25">·</span>
                  )}
                  {spec}
                </span>
              ))}
            </p>
          )}
        </div>
        {(task.downloader_id != null) && (
          <DownloadTaskActionsMenu
            task={task}
            deleting={deleting}
            replacing={replacing}
            onDelete={onDelete}
          />
        )}
      </div>
      {note && (
        <OverflowText lines={2} className="mt-1.5 text-caption leading-5 text-white/42">
          {note}
        </OverflowText>
      )}
      {percent != null && task.state !== "missing" && (
        <div className="mt-2.5 h-1.5 overflow-hidden rounded-full bg-white/[0.07]">
          <div
            className="h-full rounded-full transition-[width] duration-700"
            style={{
              width: `${Math.min(100, Math.max(percent > 0 ? 1 : 0, percent))}%`,
              backgroundColor: meta.color,
            }}
          />
        </div>
      )}
      {/* 实时传输单独一行：下行/上行速度；文件尺寸已并入规格行 */}
      {(downSpeed != null || upSpeed != null) && (
        <div className="mt-1.5 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-caption text-white/38">
          <SpeedStat direction="down" bytesPerSecond={downSpeed} />
          <SpeedStat direction="up" bytesPerSecond={upSpeed} />
        </div>
      )}
      <DownloadLifecycle task={task} ingestJob={ingestJob} variant="feed" />
      {showReplace && (
        <button
          type="button"
          onClick={() => onReplace(task)}
          disabled={replacing}
          className="mt-2 rounded-lg border border-[var(--danger)]/25 bg-[var(--danger)]/[0.07] px-2.5 py-1.5 text-caption font-semibold text-[#fee2e2] transition hover:bg-[var(--danger)]/[0.13] disabled:cursor-wait disabled:opacity-50"
        >
          {replacing ? "正在换种…" : "立即换种"}
        </button>
      )}
    </div>
  );
}

function DownloadTaskGroupCard({
  group,
  ingestJobsByHash,
  deletingTaskId,
  replacingTaskId,
  onDelete,
  onReplace,
}: {
  group: DownloadTaskGroup;
  ingestJobsByHash: Map<string, JobView>;
  deletingTaskId: string | null;
  replacingTaskId: string | null;
  onDelete: (task: DownloadTask) => void;
  onReplace: (task: DownloadTask) => void;
}) {
  if (group.mediaItemId == null) {
    const task = group.tasks[0];
    return (
      <DownloadTaskCard
        task={task}
        ingestJob={task.info_hash ? (ingestJobsByHash.get(task.info_hash.toLowerCase()) ?? null) : null}
        deleting={deletingTaskId === task.id}
        replacing={replacingTaskId === task.id}
        onDelete={onDelete}
        onReplace={onReplace}
      />
    );
  }

  const isTv = group.kind === "tv";
  const firstSubscription = group.tasks.flatMap((task) => task.subscriptions)[0];
  return (
    <article className="overflow-hidden rounded-2xl border border-white/[0.08] bg-[rgba(14,16,22,0.52)] backdrop-blur-xl">
      {/* 影片身份集中在紧凑顶部，种子列表另起整行占满卡片宽度。海报不再
          作为贯穿整组的左栏，长种子名、状态和操作按钮因此有完整横向空间。 */}
      <div className="flex items-center gap-3.5 border-b border-white/[0.07] px-4 py-3 max-md:px-3.5">
        <div className="w-10 shrink-0 max-md:w-9">
          <div className="aspect-[2/3] overflow-hidden rounded-lg bg-[#141824] ring-1 ring-white/10">
            <div className="flex size-full items-center justify-center bg-gradient-to-b from-white/[0.07] to-[#11141c] text-white/25">
              {isTv ? <TvIcon className="size-4" /> : <FilmIcon className="size-4" />}
            </div>
          </div>
        </div>
        <div className="min-w-0 flex-1">
          <h3 className="min-w-0 text-ui font-semibold text-white/90">
            <OverflowText>{group.title}</OverflowText>
          </h3>
          <p className="mt-0.5 text-caption text-white/40">
            {isTv ? "剧集" : "电影"}
            <span aria-hidden="true"> · </span>
            {group.tasks.length} 个下载资源
          </p>
        </div>
        {firstSubscription && (
          <Link to={`/subscriptions/${firstSubscription.id}`}
            className="shrink-0 rounded-lg border border-white/10 px-3 py-1.5 text-caption font-medium text-white/60 transition hover:bg-white/[0.06] hover:text-white"
          >
            查看订阅
          </Link>
        )}
      </div>
      <div className="space-y-2 p-3.5 max-md:p-3">
        {group.tasks.map((task) => (
          <DownloadTaskCard
            key={task.id}
            task={task}
            ingestJob={task.info_hash ? (ingestJobsByHash.get(task.info_hash.toLowerCase()) ?? null) : null}
            grouped
            deleting={deletingTaskId === task.id}
            replacing={replacingTaskId === task.id}
            onDelete={onDelete}
            onReplace={onReplace}
          />
        ))}
      </div>
    </article>
  );
}

function DownloadTaskCard({
  task,
  ingestJob,
  grouped = false,
  deleting,
  replacing,
  onDelete,
  onReplace,
}: {
  task: DownloadTask;
  ingestJob: JobView | null;
  grouped?: boolean;
  deleting: boolean;
  replacing: boolean;
  onDelete: (task: DownloadTask) => void;
  onReplace: (task: DownloadTask) => void;
}) {
  const ingestOwnsCardState = ingestOwnsTaskState(task, ingestJob);
  const meta =
    ingestOwnsCardState && ingestJob && ingestJob.last_status != null
      ? INGEST_STATE_META[ingestJob.last_status]
      : DOWNLOAD_STATE_META[task.state];
  const title = grouped
    ? task.name || task.media_title || task.info_hash || task.id
    : task.media_title || task.name || task.info_hash || task.id;
  const torrentName = !grouped && task.name && task.name !== title ? task.name : null;
  const sourceLabel = isSubscriptionTask(task)
    ? "订阅投递"
    : taskSourceOf(task) === "manual"
      ? "手动下载"
      : "下载器任务";
  const percent = task.progress == null ? null : Math.floor(task.progress * 100);
  const displayPercent = percent;
  const progressLabel = "下载进度";
  const showInlineReplace = shouldOfferInlineReplacement(task);
  // 与侧栏角标、「需要处理」分区同一口径（download-attention）：外部任务只观察不报警
  const needsAttention = downloadTaskNeedsAttention(task, ingestJob);
  const taskNote = downloadTaskNote(task, ingestJob);
  const showDownloadRuntime = task.state === "downloading";

  return (
    <article
      className={
        grouped
          ? "rounded-xl border border-white/[0.06] bg-black/15 p-3"
          : "rounded-2xl border border-white/[0.08] bg-[rgba(14,16,22,0.5)] p-4 backdrop-blur-xl max-md:p-3.5"
      }
    >
      <div className="min-w-0">
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 flex-1 items-center gap-2.5">
            <TaskStatusDot label={meta.label} dotClass={meta.dot} />
            <h3 className="min-w-0 text-ui font-semibold leading-5 text-white/90">
              <OverflowText>{title}</OverflowText>
            </h3>
          </div>
          {(task.downloader_id != null) && (
            <DownloadTaskActionsMenu
              task={task}
              deleting={deleting}
              replacing={replacing}
              onDelete={onDelete}
            />
          )}
        </div>
        {(torrentName || taskNote) && (
          <div
            className={`mt-2.5 text-sub leading-5 ${
              needsAttention
                ? "rounded-xl border border-[var(--danger)]/15 bg-[var(--danger)]/[0.06] px-3 py-2.5 text-[var(--danger)]"
                : "text-white/60"
            }`}
          >
            <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-2">
              <div className="min-w-0 flex-1 basis-[28rem]">
                <OverflowText
                  lines={2}
                  className="break-words"
                  tooltipContent={
                    <span className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                      {needsAttention && <span className="mr-1.5 font-semibold">需要处理：</span>}
                      {torrentName && <span>{torrentName}</span>}
                      {torrentName && taskNote && <span aria-hidden="true"> · </span>}
                      {taskNote}
                    </span>
                  }
                >
                  {needsAttention && <span className="mr-1.5 font-semibold">需要处理：</span>}
                  {torrentName && <span>{torrentName}</span>}
                  {torrentName && taskNote && <span aria-hidden="true"> · </span>}
                  {taskNote}
                </OverflowText>
              </div>
              {showInlineReplace && (
                <button
                  type="button"
                  onClick={() => onReplace(task)}
                  disabled={replacing}
                  className="shrink-0 rounded-lg border border-[var(--danger)]/30 bg-[var(--danger)]/[0.1] px-3 py-1.5 text-caption font-semibold text-[#fee2e2] transition hover:border-[var(--danger)]/45 hover:bg-[var(--danger)]/[0.16] disabled:cursor-wait disabled:opacity-55"
                >
                  {replacing ? "正在换种…" : "立即换种"}
                </button>
              )}
            </div>
          </div>
        )}
        <div className="mt-2.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-caption text-white/40">
          <span>{sourceLabel}</span>
          <span>{task.downloader_name || "所有下载器均未找到"}</span>
          {task.size_bytes != null && <span>{formatBytes(task.size_bytes)}</span>}
        </div>
      </div>
      {displayPercent != null && task.state !== "missing" && (
        <div className="mt-3">
          <div className="mb-1.5 grid grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 text-caption">
            <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5">
              <span className="font-medium text-white/55">{progressLabel}</span>
              {showDownloadRuntime && task.dlspeed_bytes != null && task.dlspeed_bytes > 0 && (
                <>
                  <span aria-hidden="true" className="text-white/25">
                    ·
                  </span>
                  <SpeedStat direction="down" bytesPerSecond={task.dlspeed_bytes} />
                </>
              )}
            </div>
            <span className="tnum shrink-0 font-semibold text-white/65">
              {Math.min(100, Math.max(0, displayPercent))}%
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-white/[0.08]">
            <div
              className="h-full rounded-full transition-[width] duration-700"
              style={{
                width: `${Math.min(100, Math.max(displayPercent > 0 ? 1 : 0, displayPercent))}%`,
                backgroundColor: meta.color,
              }}
            />
          </div>
        </div>
      )}
      <DownloadLifecycle task={task} ingestJob={ingestJob} />
      {/* 出错的是下载器里的**这条任务**，不是下载器本身；能在 crawler-media 里完成的
          动作只有删掉它——订阅任务删掉后巡检会把工单退回重新寻找资源。任务缺失
          （missing）时下载器里已经没有它，巡检会自动退回工单，不需要用户动手，
          因此不再给任何按钮 */}
      {downloadTaskNeedsAttention(task, ingestJob) && (
        <footer className="mt-3 flex flex-wrap items-center justify-end gap-2 border-t border-white/[0.06] pt-3">
          {task.state === "error" && task.downloader_id != null && (
            <button
              type="button"
              onClick={() => onDelete(task)}
              disabled={deleting}
              className="rounded-lg border border-[var(--danger)]/25 bg-[var(--danger)]/[0.07] px-3 py-1.5 text-caption font-medium text-[var(--danger)] transition hover:bg-[var(--danger)]/[0.14] disabled:cursor-wait disabled:opacity-55"
            >
              {deleting ? "删除中…" : "删除下载任务"}
            </button>
          )}
        </footer>
      )}
    </article>
  );
}

type LifecycleTone = "done" | "current" | "waiting" | "attention" | "future";

interface LifecycleStep {
  label: string;
  detail: string;
  tone: LifecycleTone;
}

const LIFECYCLE_DOT_STYLE: Record<LifecycleTone, string> = {
  done: "bg-[var(--ok)]",
  current: "animate-pulse bg-[var(--info)] ring-2 ring-[var(--info)]/15",
  waiting: "bg-[var(--warn)] ring-2 ring-[var(--warn)]/10",
  attention: "bg-[var(--danger)] ring-2 ring-[var(--danger)]/15",
  future: "border border-white/25",
};

const LIFECYCLE_LABEL_STYLE: Record<LifecycleTone, string> = {
  done: "text-white/60",
  current: "text-[var(--info)]",
  waiting: "text-amber-100/70",
  attention: "text-[var(--danger)]",
  future: "text-white/30",
};

const LIFECYCLE_DETAIL_STYLE: Record<LifecycleTone, string> = {
  done: "text-white/30",
  current: "text-white/30",
  waiting: "text-amber-100/40",
  attention: "text-[var(--danger)]/70",
  future: "text-white/30",
};

/** Tailwind 要静态类名，列数只能查表拿——步骤数由落点配置决定（3～6 步）。 */
const GRID_COLS_BY_STEPS: Record<number, string> = {
  3: "grid-cols-3",
  4: "grid-cols-4",
  5: "grid-cols-5",
  6: "grid-cols-6",
};

/**
 * 把下载器状态投影成同一条业务过程。这里不生成新状态：每一步都能回溯到下载
 * 快照或逐集工单，因此刷新、重试与回退仍由原领域负责。
 *
 * 新契约不再把入库 Job 与下载任务按 infohash 关联（JobDef 无 resources 字段），
 * ingestJob 恒为 null；只展示下载快照已知的状态。
 */
function DownloadLifecycle({
  task,
  ingestJob,
  variant = "card",
}: {
  task: DownloadTask;
  ingestJob: JobView | null;
  variant?: "card" | "feed";
}) {
  void ingestJob;
  // 下载轴只认下载器：存在入库 Job 不等于下载完成——边下边入库会在 19% 时就
  // 搬走已完整落盘的那一集
  const downloaded = task.state === "completed";
  const downloadAttention = task.state === "error" || task.state === "missing";
  const downloadWaiting = ["stalled", "paused", "queued", "unknown"].includes(task.state);
  const percent = task.progress == null ? null : Math.floor(task.progress * 100);
  const source = task.downloader_name || "下载器";
  let downloadStep: LifecycleStep;
  if (downloadAttention) {
    downloadStep = {
      label: DOWNLOAD_STATE_META[task.state].label,
      // error 是下载器里这条任务出了问题（多半是文件缺失），下载器本身并没有坏，
      // 不能说"等待下载器恢复"——那会把用户引去检查一个正常的下载器
      detail: task.state === "missing" ? "等待恢复关联" : "需要在下载器中处理",
      tone: "attention",
    };
  } else if (downloaded) {
    downloadStep = {
      label: "下载完成",
      detail: task.size_bytes == null ? "文件已就绪" : formatBytes(task.size_bytes),
      tone: "done",
    };
  } else {
    downloadStep = {
      label: DOWNLOAD_STATE_META[task.state].label,
      detail: percent == null ? "读取实时进度" : `${percent}%`,
      tone: downloadWaiting ? "waiting" : "current",
    };
  }

  const nextStep: LifecycleStep = downloaded
    ? { label: "等待入库", detail: "等待自动入库或媒体库扫描收尾", tone: "future" }
    : { label: "等待下载完成", detail: "下一步", tone: "future" };
  const steps: LifecycleStep[] = [
    { label: "已投递", detail: source, tone: "done" },
    downloadStep,
    nextStep,
  ];
  const feed = variant === "feed";

  return (
    <ol
      aria-label="任务完整过程"
      className={
        feed
          ? "mt-3 space-y-1.5"
          : // 步骤数随落点配置变化（监听规则多一步搬运、洗版再多一步替换），
            // 列数跟着走；窄屏统一竖排，不挤
            `mt-3 grid gap-2 border-t border-white/[0.06] pt-3 max-md:grid-cols-1 ${
              GRID_COLS_BY_STEPS[Math.min(steps.length, 6)] ?? "grid-cols-3"
            }`
      }
    >
      {steps.map((step, index) => (
        <li
          key={`${step.label}:${step.detail}`}
          className="relative flex min-w-0 items-start gap-2"
        >
          {index < steps.length - 1 && (
            <span
              aria-hidden="true"
              className={`absolute bottom-[-0.5rem] left-[0.21875rem] top-3 w-px bg-white/[0.1] ${
                feed ? "block" : "hidden max-md:block"
              }`}
            />
          )}
          <span
            aria-hidden="true"
            className={`mt-1.5 size-2 shrink-0 rounded-full ${LIFECYCLE_DOT_STYLE[step.tone]}`}
          />
          <span className={feed ? "flex min-w-0 flex-wrap items-baseline gap-x-2" : "min-w-0"}>
            <span className={`text-caption font-medium ${LIFECYCLE_LABEL_STYLE[step.tone]}`}>
              {step.label}
            </span>
            <OverflowText
              lines={step.tone === "attention" ? 2 : 1}
              className={`${feed ? "text-caption" : "mt-0.5 text-micro"} ${LIFECYCLE_DETAIL_STYLE[step.tone]}`}
            >
              {step.detail}
            </OverflowText>
          </span>
        </li>
      ))}
    </ol>
  );
}

/** 删除收进右上角菜单；查看订阅由分组标题承担，不在此重复。 */
function DownloadTaskActionsMenu({
  task,
  deleting,
  replacing,
  onDelete,
}: {
  task: DownloadTask;
  deleting: boolean;
  replacing: boolean;
  onDelete: (task: DownloadTask) => void;
}) {
  return (
    <TaskActionsMenu
      ariaLabel={`${task.name || task.info_hash}的更多操作`}
      disabled={deleting || replacing}
      items={[
        ...(task.downloader_id != null
          ? [
              {
                id: "delete",
                label: deleting ? "删除中…" : "删除任务",
                onSelect: () => onDelete(task),
                disabled: deleting,
                tone: "danger" as const,
              },
            ]
          : []),
      ]}
    />
  );
}

function downloadTaskNote(
  task: DownloadTask,
  ingestJob: JobView | null,
): string | null {
  void ingestJob;
  if (task.state === "error") {
    // 下载器给出的具体原因（文件缺失 / errorString）必须放在最前——它回答的是
    // "错在哪"；换源提示只是"还能怎么办"，不能把原因盖掉。非订阅投递的任务再补
    // 一句它不归 crawler-media 管，免得用户在这里找不到处理入口
    const reason = "下载器报告该任务出错，请在下载器中查看具体原因";
    return isSubscriptionTask(task) ? reason : `${reason}（非订阅投递的任务，仅观察）`;
  }
  if (task.state === "missing") {
    return "已投递记录仍在，但下载器中找不到对应任务；系统确认后会自动退回重新寻找资源";
  }
  if (task.state === "completed") return "下载已完成，等待自动入库或媒体库扫描收尾";
  if (task.state === "paused") return "已在下载器中暂停";
  if (task.state === "queued") return "下载器正在排队，暂停无进度计时";
  if (task.state === "checking") return "下载器正在校验数据，暂停无进度计时";
  if (task.state === "stalled") return "暂无可用连接，等待继续下载";
  if (task.state === "unknown") return "下载器未返回可识别的状态";
  return null;
}

function EmptyView({ view }: { view: TaskCenterViewName }) {
  const copy: Record<TaskCenterViewName, { title: string; note: string }> = {
    all: { title: "当前没有任务", note: "新的后台作业或下载任务会自动出现在这里。" },
    attention: { title: "当前无需处理", note: "异常或需要确认的任务会优先出现在这里。" },
    active: { title: "当前没有进行中的任务", note: "新任务启动后会自动进入实时过程。" },
    history: {
      title: "还没有历史记录",
      note: "完成、取消，以及被你忽略的后台作业都会保留在这里。",
    },
  };
  return (
    <div className="mt-8 rounded-2xl border border-white/[0.07] bg-black/20 px-6 py-16 text-center backdrop-blur-xl">
      <p className="text-title-sm font-semibold text-white/75">{copy[view].title}</p>
      <p className="mt-2 text-ui text-[var(--text-muted)]">{copy[view].note}</p>
    </div>
  );
}
