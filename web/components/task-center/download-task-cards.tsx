"use client";

import { Link } from "react-router-dom";

import { TaskStatusDot } from "@/components/job-center";
import { FilmIcon, TvIcon } from "@/components/icons";
import { OverflowText } from "@/components/overflow-text";
import type { DownloadTask } from "@/lib/api/downloaders";
import type { JobView } from "@/lib/api/jobs";
import { shouldOfferInlineReplacement } from "@/lib/download-task-actions";
import { formatBytes } from "@/lib/format";
import {
  downloadTaskNeedsAttention,
  isSubscriptionTask,
  taskSourceOf,
  type DownloadTaskGroup,
} from "@/lib/task-activity";

import {
  DOWNLOAD_STATE_META,
  DownloadLifecycle,
  DownloadTaskActionsMenu,
  downloadTaskNote,
  INGEST_STATE_META,
  ingestOwnsTaskState,
} from "./download-lifecycle";
import { SpeedStat } from "./task-layout";

/** “现在”区域使用过程行，不再把下载卡片原样塞进时间轴。 */
export function DownloadTaskGroupFeed({
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
          className="mt-2 rounded-lg border border-[var(--danger)]/25 bg-[var(--danger)]/[0.07] px-2.5 py-1.5 text-caption font-semibold text-[var(--danger)] transition hover:bg-[var(--danger)]/[0.13] disabled:cursor-wait disabled:opacity-50"
        >
          {replacing ? "正在换种…" : "立即换种"}
        </button>
      )}
    </div>
  );
}

export function DownloadTaskGroupCard({
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
    <article className="card-row-surface overflow-hidden rounded-2xl">
      {/* 影片身份集中在紧凑顶部，种子列表另起整行占满卡片宽度。海报不再
          作为贯穿整组的左栏，长种子名、状态和操作按钮因此有完整横向空间。 */}
      <div className="flex items-center gap-3.5 border-b border-white/[0.07] px-4 py-3 max-md:px-3.5">
        <div className="w-10 shrink-0 max-md:w-9">
          <div className="dark-media-surface aspect-[2/3] overflow-hidden rounded-lg bg-[#141824] ring-1 ring-white/10">
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
          ? "rounded-xl border border-[var(--line)] bg-[var(--surface-inset)] p-3"
          : "card-row-surface rounded-2xl p-4 max-md:p-3.5"
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
                  className="shrink-0 rounded-lg border border-[var(--danger)]/30 bg-[var(--danger)]/[0.1] px-3 py-1.5 text-caption font-semibold text-[var(--danger)] transition hover:border-[var(--danger)]/45 hover:bg-[var(--danger)]/[0.16] disabled:cursor-wait disabled:opacity-55"
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
