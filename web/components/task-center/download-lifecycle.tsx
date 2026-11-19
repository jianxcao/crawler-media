"use client";

import { TaskActionsMenu } from "@/components/job-center";
import { OverflowText } from "@/components/overflow-text";
import type { DownloadTask } from "@/lib/api/downloaders";
import type { JobStatus, JobView } from "@/lib/api/jobs";
import { formatBytes } from "@/lib/format";
import { jobNeedsAttention } from "@/lib/job-attention";
import { isSubscriptionTask, type DownloadTaskGroup } from "@/lib/task-activity";

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
export interface TaskStateMeta {
  label: string;
  color: string;
  dot: string;
}

export const DOWNLOAD_STATE_META: Record<DownloadTask["state"], TaskStateMeta> = {
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

export const INGEST_STATE_META: Record<JobStatus, TaskStateMeta> = {
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
export function ingestOwnsTaskState(task: DownloadTask, ingestJob: JobView | null): boolean {
  if (ingestJob == null || ingestJob.last_status === "cancelled") return false;
  // 用户已经忽略的失败入库不再顶到最前——他要的正是"别再拿这件事挡着我"，
  // 卡片交回下载轴如实讲进度。
  if (jobNeedsAttention(ingestJob)) return true;
  return task.state === "completed";
}

export function downloadGroupTimelineTone(
  group: DownloadTaskGroup,
  ingestJobsByHash: Map<string, JobView>,
): "active" | "waiting" {
  const executing = group.tasks.some((task) => {
    const ingestJob = task.info_hash ? ingestJobsByHash.get(task.info_hash.toLowerCase()) : undefined;
    return ingestJob?.last_status === "running" || ["downloading", "checking"].includes(task.state);
  });
  return executing ? "active" : "waiting";
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
export function DownloadLifecycle({
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
export function DownloadTaskActionsMenu({
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

export function downloadTaskNote(
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
