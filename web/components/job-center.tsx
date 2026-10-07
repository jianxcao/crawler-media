"use client";

import { useState } from "react";

import { useNavigate } from "react-router-dom";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import {
  ActivityIcon,
  ChevronRightIcon,
  MoreIcon,
} from "@/components/icons";
import { Modal } from "@/components/modal";
import { OverflowText } from "@/components/overflow-text";
import {
  cancelJob,
  dismissJob,
  undismissJob,
  type JobStatus,
  type JobView,
} from "@/lib/api/jobs";
import {
  isDismissed,
  markJobDismissed,
  markJobUndismissed,
  ACTIVE_FEED_JOB_STATUSES,
  ATTENTION_JOB_STATUSES,
} from "@/lib/job-attention";
import { formatBytes } from "@/lib/format";
import { useJobs } from "@/lib/jobs";
import { taskActivityBadge, useTaskActivity } from "@/lib/task-activity";
import { formatDateTime, formatRelativeTime } from "@/lib/time";
import { usePermissions } from "@/lib/permissions";

export const JOB_TYPE_LABELS: Record<string, string> = {
  "subtitle.generate": "生成 AI 字幕",
  "library.scan": "扫描媒体库",
  "library.metadata.refresh": "刷新媒体库元数据",
  "media.metadata.refresh": "刷新条目元数据",
  "library.chapter_images": "生成章节",
  "library.organize": "整理媒体库文件",
  "library.transfer": "转移媒体库条目",
  "library.ingest": "自动整理入库",
  subscribe_search: "订阅搜索",
  subscribe_rss: "RSS 抓取",
  transfer: "文件转移",
  scrape: "元数据刮削",
  check_in: "站点签到",
  catalog_refresh: "刷新目录元数据",
  watch_intake: "目录监控入库",
};

export const JOB_STATUS_LABELS: Record<string, string> = {
  queued: "排队中",
  running: "进行中",
  retry_wait: "等待重试",
  cancelling: "正在取消",
  waiting: "等待前置任务",
  blocked: "需要处理",
  succeeded: "已完成",
  failed: "未完成",
  cancelled: "已取消",
};

const LANGUAGE_LABELS: Record<string, string> = {
  chs: "简体中文",
  cht: "繁体中文",
  eng: "英语",
  jpn: "日语",
  kor: "韩语",
  fre: "法语",
  ger: "德语",
  spa: "西班牙语",
  ita: "意大利语",
  por: "葡萄牙语",
  rus: "俄语",
  tha: "泰语",
};

const STATUS_STYLE: Record<JobStatus, { dot: string; border?: string }> = {
  queued: { dot: "bg-white/40" },
  running: { dot: "animate-pulse bg-[var(--info)]" },
  succeeded: { dot: "bg-[var(--ok)]" },
  failed: {
    dot: "bg-[var(--danger)]",
    border: "border-[var(--danger)]/20",
  },
  cancelled: { dot: "bg-white/30" },
};

function detailNumber(details: Record<string, unknown>, key: string): number | null {
  const value = details[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function detailString(details: Record<string, unknown>, key: string): string | null {
  const value = details[key];
  return typeof value === "string" && value ? value : null;
}

function detailCount(details: Record<string, unknown>, key: string): number {
  const value = details[key];
  if (Array.isArray(value)) return value.length;
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

/** 新契约 JobDef 只有 payload（任务入参），运行期 details 不再下发；按键读入参。 */
function jobString(job: JobView, key: string): string | null {
  return detailString(job.payload, key);
}

/** 字幕任务的目标语言来自已确认输入，运行期 details 只作为更新后的优先快照。 */
function subtitleLanguageLabel(job: JobView): string | null {
  if (job.kind !== "subtitle.generate") return null;
  const target = jobString(job, "target_language");
  if (!target) return null;
  const primary = LANGUAGE_LABELS[target] ?? target;
  const secondary = jobString(job, "secondary_language");
  return secondary
    ? `${primary} + ${LANGUAGE_LABELS[secondary] ?? secondary}（双语）`
    : primary;
}

function originLabel(job: JobView): string {
  // 新契约 JobDef 无 origin/actor_name；有调度定义（schedule）的任务视为系统自动。
  return job.schedule ? "系统自动" : "后台任务";
}

/** 终态缺 message 时的兜底文案。 */
function statusSummaryFallback(status: JobStatus | null): string {
  switch (status) {
    case "cancelled":
      return "任务已取消";
    case "failed":
      return "任务未完成";
    case "succeeded":
      return "任务已完成";
    default:
      return "等待执行";
  }
}

function jobDetailItems(job: JobView): Array<{ label: string; alert?: boolean }> {
  const payload = job.payload;
  const items: Array<{ label: string; alert?: boolean }> = [];

  if (job.kind === "subtitle.generate") {
    const language = subtitleLanguageLabel(job);
    const parallelism = detailNumber(payload, "parallelism");
    if (language) items.push({ label: LANGUAGE_LABELS[language] ?? language });
    if (payload.uses_ocr === true) items.push({ label: "PGS OCR" });
    if (parallelism && parallelism > 1) items.push({ label: `${parallelism} 路并行` });
  } else if (job.kind === "library.scan") {
    const identified = detailNumber(payload, "identified");
    const unidentified = detailNumber(payload, "unidentified");
    const probed = detailNumber(payload, "probed");
    const missing = detailNumber(payload, "marked_missing");
    const errors = detailCount(payload, "errors");
    if (identified) items.push({ label: `识别 ${identified} 个` });
    if (probed) items.push({ label: `补探 ${probed} 个` });
    if (unidentified) items.push({ label: `待识别 ${unidentified} 个`, alert: true });
    if (missing) items.push({ label: `缺失 ${missing} 个`, alert: true });
    if (errors) items.push({ label: `${errors} 个问题`, alert: true });
  } else if (job.kind.includes("metadata.refresh")) {
    const failed = detailNumber(payload, "failed");
    if (failed) items.push({ label: `${failed} 个未完成`, alert: true });
  } else if (job.kind === "library.organize") {
    const errors = detailCount(payload, "errors");
    if (errors) items.push({ label: `${errors} 个问题`, alert: true });
  } else if (job.kind === "library.transfer") {
    const moved = detailNumber(payload, "bytes_moved");
    const totalBytes = detailNumber(payload, "total_bytes");
    if (moved) items.push({ label: `已搬运 ${formatBytes(moved)}` });
    else if (totalBytes) items.push({ label: `共 ${formatBytes(totalBytes)}` });
  } else if (job.kind === "library.ingest") {
    const fileName = detailString(payload, "file_name");
    if (fileName) items.push({ label: fileName });
  }

  return items.slice(0, 4);
}

/** 任务中心统一状态点：紧贴标题展示状态，完整文字通过 title 与无障碍标签保留。 */
export function TaskStatusDot({
  label,
  dotClass,
}: {
  label: string;
  dotClass: string;
}) {
  return (
    <span
      title={label}
      role="img"
      aria-label={`状态：${label}`}
      className={`size-2.5 shrink-0 rounded-full ${dotClass}`}
    >
      <span className="sr-only">{label}</span>
    </span>
  );
}

interface TaskActionMenuItem {
  id: string;
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  tone?: "default" | "danger";
}

/** 标准任务卡片操作入口：固定在右上角，避免不同任务类型各自发明布局。 */
export function TaskActionsMenu({
  ariaLabel,
  disabled = false,
  items,
}: {
  ariaLabel: string;
  disabled?: boolean;
  items: TaskActionMenuItem[];
}) {
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-sub font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[disabled]:pointer-events-none " +
    "data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label={ariaLabel}
          disabled={disabled}
          className="glass-row flex size-7 !w-7 items-center justify-center p-0 text-white/55 data-[state=open]:!bg-[var(--glass-fill-active)] data-[state=open]:!text-[var(--text)] disabled:opacity-40"
        >
          <MoreIcon className="size-4 max-md:size-5" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[9rem] p-1"
        >
          {items.map((item) => (
            <DropdownMenu.Item
              key={item.id}
              onSelect={item.onSelect}
              disabled={item.disabled}
              className={`${itemClass} ${
                item.tone === "danger"
                  ? "!text-[#ff6b6b] data-[highlighted]:!bg-[#ff6b6b]/10"
                  : "text-white/75"
              }`}
            >
              {item.label}
            </DropdownMenu.Item>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function StatusDot({ status }: { status: JobStatus | null }) {
  const style = status ? STATUS_STYLE[status] : undefined;
  return (
    <TaskStatusDot
      label={status ? (JOB_STATUS_LABELS[status] ?? status) : "未知状态"}
      dotClass={style?.dot ?? "bg-white/40"}
    />
  );
}

export function JobCard({ job, onNavigate }: { job: JobView; onNavigate: () => void }) {
  const { refresh, upsert } = useJobs();
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [dismissOpen, setDismissOpen] = useState(false);
  const status = job.last_status;
  const active = status != null && ACTIVE_FEED_JOB_STATUSES.has(status);
  const executing = status === "running";
  // 「需要处理」是**当下要不要用户动手**，不是"这件事失败过"。用户已经拍板
  // 不处理，红框与「需要处理：」前缀就该一起撤下——否则忽略了还在报警，
  // 等于没忽略。失败这个事实由状态点与摘要文案继续如实承担。
  const attention =
    status != null && ATTENTION_JOB_STATUSES.has(status) && !isDismissed(job);
  const cancellable = active;
  // 忽略只开给失败任务：其余状态各有正确出口，不该被"藏起来"顶替。活跃任务
  // 要真正终结得走取消——它们仍占着去重键与资源锁，光藏掉提醒会留下一个谁也
  // 看不见、却挡着同名任务再次创建的幽灵。
  const dismissable = status === "failed" && !isDismissed(job);
  const dismissed = isDismissed(job);
  const statusStyle = status ? STATUS_STYLE[status] : undefined;
  const details = jobDetailItems(job);
  const hasDomainAlert = details.some((item) => item.alert);
  const compact =
    (status === "succeeded" || status === "cancelled") && !hasDomainAlert;
  const cardBorder =
    statusStyle?.border ?? (hasDomainAlert ? "border-amber-300/15" : "border-white/[0.08]");
  const jobTypeLabel = JOB_TYPE_LABELS[job.kind] || job.kind;
  const title = job.friendly_name || job.name || jobTypeLabel;
  const completedSubtitleLanguage =
    status === "succeeded" ? subtitleLanguageLabel(job) : null;
  const summaryMessage = completedSubtitleLanguage
    ? `已生成${completedSubtitleLanguage}字幕`
    : (job.last_error ?? jobString(job, "message") ?? statusSummaryFallback(status));

  async function dismissCurrentJob() {
    setBusyAction("dismiss");
    setActionError(null);
    try {
      upsert(await dismissJob(job.id));
      // 新契约 /dismiss 退化为空操作，忽略状态由组件层维护（见 lib/job-attention）
      markJobDismissed(job.id, job.last_finished_at);
      setDismissOpen(false);
    } catch (error) {
      setActionError((error as Error).message || "忽略失败，请稍后重试");
    } finally {
      setBusyAction(null);
    }
  }

  async function undismissCurrentJob() {
    setBusyAction("undismiss");
    setActionError(null);
    try {
      upsert(await undismissJob(job.id));
      markJobUndismissed(job.id);
    } catch (error) {
      setActionError((error as Error).message || "撤销忽略失败，请稍后重试");
    } finally {
      setBusyAction(null);
    }
  }

  async function cancelCurrentJob() {
    setBusyAction("cancel");
    setActionError(null);
    try {
      await cancelJob(job.id);
      refresh();
    } catch (error) {
      setActionError((error as Error).message || "取消任务失败，请稍后重试");
    } finally {
      setBusyAction(null);
    }
  }

  const menuItems: TaskActionMenuItem[] = [
    ...(cancellable
      ? [
          {
            id: "cancel",
            label: busyAction === "cancel" ? "正在取消…" : "取消任务",
            onSelect: () => void cancelCurrentJob(),
          },
        ]
      : []),
    // 「忽略」排在补救动作之后：先给出路（重试 / 看日志 / 交给 Agent），
    // 最后才给出口。这是用户在此前唯一缺的那一项——失败任务没有任何办法
    // 从「需要处理」里下架，红角标于是永不熄灭（issue #221）。
    ...(dismissable
      ? [
          {
            id: "dismiss",
            label: "忽略这个任务",
            onSelect: () => setDismissOpen(true),
          },
        ]
      : []),
    ...(dismissed
      ? [
          {
            id: "undismiss",
            label: busyAction === "undismiss" ? "撤销中…" : "撤销忽略",
            onSelect: () => void undismissCurrentJob(),
          },
        ]
      : []),
  ];

  return (
    <article
      className={`overflow-hidden rounded-2xl border bg-[rgba(14,16,22,0.52)] ${cardBorder} ${compact ? "px-3.5 py-2.5" : "p-4"}`}
    >
      <div className="min-w-0">
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 flex-1 items-center gap-2.5">
            <StatusDot status={status} />
            {job.name && (
              <span className="shrink-0 rounded-md border border-white/[0.08] bg-white/[0.045] px-1.5 py-0.5 text-micro font-medium text-white/55">
                {jobTypeLabel}
              </span>
            )}
            <h3 className="min-w-0 text-ui font-semibold leading-5 text-white/90">
              <OverflowText>{title}</OverflowText>
            </h3>
          </div>
          {menuItems.length > 0 && (
            <TaskActionsMenu
              ariaLabel={`${title}的更多操作`}
              disabled={busyAction !== null}
              items={menuItems}
            />
          )}
        </div>

        <div
          className={`${compact ? "mt-1 text-caption leading-5" : "mt-2.5 text-sub leading-5"} ${
            attention
              ? "rounded-xl border border-[var(--danger)]/15 bg-[var(--danger)]/[0.06] px-3 py-2.5 text-[var(--danger)]"
              : compact
                ? "text-white/50"
                : "text-white/60"
          }`}
        >
          <OverflowText
            lines={compact ? 1 : 2}
            className={compact ? "break-words" : "min-h-10 break-words"}
            tooltipContent={
              <span className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                {attention && <span className="mr-1.5 font-semibold">需要处理：</span>}
                {summaryMessage}
              </span>
            }
          >
            {attention && <span className="mr-1.5 font-semibold">需要处理：</span>}
            {summaryMessage}
          </OverflowText>
        </div>

        {!compact && executing && (
          <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-white/[0.07]">
            <div className="h-full w-1/3 animate-pulse rounded-full bg-[var(--info)]" />
          </div>
        )}

        {!compact && details.length > 0 && (
          <div className="mt-3 flex flex-wrap gap-1.5">
            {details.map((item) => (
              <span
                key={item.label}
                className={`min-w-0 max-w-full rounded-md border px-2 py-1 text-caption ${
                  item.alert
                    ? "border-amber-300/15 bg-amber-300/[0.05] text-amber-100/75"
                    : "border-white/[0.07] bg-white/[0.035] text-white/45"
                }`}
              >
                <OverflowText>{item.label}</OverflowText>
              </span>
            ))}
          </div>
        )}

        {actionError && (
          <p className="mt-2 text-caption leading-5 text-[#ff9f9f]">{actionError}</p>
        )}
      </div>
      <footer className={`${compact ? "mt-1.5" : "mt-3"} flex flex-wrap items-center gap-2`}>
        <OverflowText
          alwaysShowTooltip
          tooltipContent={`${originLabel(job)} · ${formatDateTime(
            job.last_finished_at ?? job.next_run_after,
          )}`}
          className="text-micro text-white/25"
        >
          {originLabel(job)} · {formatRelativeTime(job.last_finished_at ?? job.next_run_after)}
        </OverflowText>
        {dismissed && (
          <span className="ml-auto shrink-0 rounded-md border border-white/[0.08] bg-white/[0.04] px-1.5 py-0.5 text-micro text-white/40">
            已忽略
          </span>
        )}
      </footer>
      {dismissOpen && (
        <DismissJobDialog
          job={job}
          busy={busyAction === "dismiss"}
          onClose={() => {
            if (busyAction !== "dismiss") setDismissOpen(false);
          }}
          onConfirm={() => void dismissCurrentJob()}
        />
      )}
    </article>
  );
}

/**
 * 忽略的二次确认。
 *
 * 新契约没有「静音自动来源」位（旧 /dismiss 的 muteSource 语义随旧契约下线），
 * 弹窗只确认「忽略这一条」：任务从「需要处理」下架，移到「已结束」，随时可撤销。
 */
function DismissJobDialog({
  job,
  busy,
  onClose,
  onConfirm,
}: {
  job: JobView;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const title = job.name || JOB_TYPE_LABELS[job.kind] || job.kind;

  return (
    <Modal open onClose={busy ? () => {} : onClose} label="忽略任务" topmost>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-white">忽略这个任务？</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          它会从「需要处理」下架，移到「已结束」。任务记录、失败原因和重试入口都还在，
          随时可以撤销忽略。
        </p>
        <p className="mt-3 break-words rounded-xl border border-white/[0.08] bg-white/[0.035] px-3.5 py-3 text-sub leading-6 text-white/75">
          {title}
        </p>
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
            onClick={onConfirm}
            disabled={busy}
            className="rounded-lg bg-white/90 px-4 py-2 text-ui font-medium text-black transition hover:bg-white disabled:opacity-40"
          >
            {busy ? "正在忽略…" : "忽略"}
          </button>
        </div>
      </div>
    </Modal>
  );
}

/**
 * 侧栏「活动」入口。
 *
 * 角标只表达**当前最该被看见的那件事**，点击就直达那件事：有失败/阻塞的任务
 * 时亮警示色并落到任务视角的「需要处理」，只有进行中任务时落到「进行中」。
 * 角标数的是任务，落点就必须是任务视角对应的那一片——把人丢到「观看」再让他
 * 自己找"为什么有提醒"，等于让提醒自证失败。角标为空时才回到页面默认的观看。
 *
 * 计数走 useTaskActivity 这份共享口径，与活动页里的选项卡数字保证一致。
 */
export function JobCenter({ collapsed, active = false }: { collapsed: boolean; active?: boolean }) {
  const navigate = useNavigate();
  const { isAdmin } = usePermissions();
  const activity = useTaskActivity();
  if (!isAdmin) return null;
  const { alert, count: badgeCount, href, hint } = taskActivityBadge(activity);
  return (
    <button
      type="button"
      onClick={() => navigate(href)}
      data-active={active}
      title={collapsed ? `活动（${hint}）` : undefined}
      className={`glass-row nav-item py-2 max-md:py-2.5 ${collapsed ? "justify-center px-0" : "px-3"}`}
    >
      <span className="relative shrink-0">
        <ActivityIcon className="size-[18px] max-md:size-[22px]" />
        {badgeCount > 0 && (
          <span
            className={`absolute -right-1 -top-1 size-2 rounded-full ${
              alert ? "bg-[var(--danger)]" : "bg-[var(--info)]"
            }`}
          />
        )}
      </span>
      {!collapsed && (
        <>
          <span className="flex-1 text-ui font-medium">活动</span>
          {badgeCount > 0 && (
            <span
              className={`rounded-full px-1.5 py-0.5 text-[11px] font-semibold leading-none ${
                alert
                  ? "bg-[var(--danger)]/20 text-[var(--danger)]"
                  : "bg-[var(--info)]/20 text-[var(--info)]"
              }`}
            >
              {badgeCount}
            </span>
          )}
        </>
      )}
    </button>
  );
}
