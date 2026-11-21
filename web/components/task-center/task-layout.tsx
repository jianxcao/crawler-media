"use client";

import { useState, type ReactNode } from "react";

import { Link } from "react-router-dom";

import { CheckIcon, InfoIcon, XIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import type { DownloadTask } from "@/lib/api/downloaders";
import { formatBytes } from "@/lib/format";
import type { TaskCenterViewName } from "@/lib/task-center";

/** 删除任务的二次确认：安全默认只移除任务，是否删除数据文件由用户显式选择。 */
export function DeleteDownloadTaskDialog({
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

export function SourceWarning({ error }: { error: string }) {
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
export function TaskAttentionSection({
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
  children: ReactNode;
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
export function TaskTimelineSection({
  title,
  count,
  children,
}: {
  title: string;
  count: number;
  children: ReactNode;
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
export function TaskTimelineItem({
  tone = "active",
  children,
}: {
  tone?: "active" | "waiting" | "success" | "cancelled";
  children: ReactNode;
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

/**
 * 实时速度的统一配色：上传走 --ok 绿（做种的"战果"），下载走 --info 蓝（"正在进行"），
 * 数值加粗从灰色标签里跳出来。所有来自下载器（qB/Tr）的任务——刷流汇总、刷流单行、
 * 普通任务的实时行——都走这一个组件，保证任务中心里 ↑/↓ 的颜色语义处处一致。
 * glow 只给刷流汇总头部用（折叠时也要一眼可见）；speed 为 0/空时给破折号或灰字占位，
 * 由调用方决定是否渲染占位（固定列需要占位防塌陷，自由流式行则直接不渲染）。
 */
export function SpeedStat({
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

export function EmptyView({ view }: { view: TaskCenterViewName }) {
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
