import type { TransferFileItem, TransferGroupItem } from "@/lib/api/transfers";
import { formatBytes, formatVideoResolution } from "@/lib/format";
import { CopyButton } from "@/components/copy-button";
import {
  ChevronDownIcon,
  ClockIcon,
  FilmIcon,
  FolderIcon,
  RefreshIcon,
  SparkIcon,
  TrashIcon,
  TvIcon,
} from "@/components/icons";
import {
  formatEpisodeTag,
  formatItemDuration,
  formatSummaryDuration,
  transferModeBadgeMeta,
} from "./transfer-helpers";

export interface ExpandedGroupState {
  open: boolean;
  loading: boolean;
  items: TransferFileItem[];
  hasMore: boolean;
}

interface TransferGroupCardProps {
  group: TransferGroupItem;
  expandedState?: ExpandedGroupState;
  isRefreshing: boolean;
  retransferringId: string | null;
  deleteDisabled: boolean;
  onToggleExpand: (group: TransferGroupItem) => void;
  onLoadMoreGroupFiles: (group: TransferGroupItem) => void;
  onRefreshGroupMetadata: (group: TransferGroupItem) => void;
  onRetransferFile: (item: TransferFileItem) => void;
  onDeleteFile: (item: TransferFileItem) => void;
}

export function TransferGroupCard({
  group,
  expandedState,
  isRefreshing,
  retransferringId,
  deleteDisabled,
  onToggleExpand,
  onLoadMoreGroupFiles,
  onRefreshGroupMetadata,
  onRetransferFile,
  onDeleteFile,
}: TransferGroupCardProps) {
  const isOpen = Boolean(expandedState?.open);
  const modeMeta = transferModeBadgeMeta(group.primary_mode);
  const isAllStrm = group.strm_count > 0 && group.strm_count === group.file_count;
  const isMixedMode =
    !isAllStrm &&
    group.strm_count > 0 &&
    group.strm_count < group.file_count;
  const durationLabel =
    group.total_duration_ms > 0 ? formatSummaryDuration(group.total_duration_ms) : null;

  return (
    <div className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4 sm:p-5 transition hover:border-white/[0.14] hover:bg-white/[0.03]">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="inline-flex size-6 shrink-0 items-center justify-center rounded-lg bg-white/[0.06] text-white/75">
              {group.media_kind === "tv" ? (
                <TvIcon className="size-3.5 text-sky-300" />
              ) : (
                <FilmIcon className="size-3.5 text-amber-300" />
              )}
            </span>

            <span className="text-sm sm:text-base font-semibold text-[var(--text)]">
              {group.media_title || "未命名作品"}
            </span>

            <span className="rounded-md border border-white/[0.1] bg-white/[0.05] px-2 py-0.5 text-xs font-medium text-white/85">
              {group.file_count.toLocaleString("zh-CN")} 个文件
            </span>

            <span className={`rounded-md px-2 py-0.5 text-xs font-medium ${modeMeta.className}`}>
              {isMixedMode
                ? `混合 (${group.strm_count} STRM)`
                : modeMeta.shortLabel}
            </span>

            {group.primary_resolution && (
              <span className="rounded-md bg-white/[0.06] px-2 py-0.5 text-xs font-mono text-white/80">
                {formatVideoResolution(group.primary_resolution)}
              </span>
            )}

            {group.local_size_bytes > 0 ? (
              <span className="text-xs font-mono text-[var(--text-muted)]">
                {formatBytes(group.local_size_bytes)}
              </span>
            ) : (
              group.strm_count > 0 && (
                <span className="text-xs text-purple-300/80">云端直链</span>
              )
            )}

            {durationLabel && (
              <span className="inline-flex items-center gap-1 text-xs text-[var(--text-faint)]">
                <ClockIcon className="size-3" />
                {durationLabel}
              </span>
            )}
          </div>

          {group.sample_dir && (
            <div className="mt-2 flex items-center gap-1.5 text-xs text-[var(--text-muted)]">
              <FolderIcon className="size-3.5 shrink-0 text-sky-400/70" />
              <span className="truncate font-mono text-[11px]" title={group.sample_dir}>
                {group.sample_dir}
              </span>
              <CopyButton
                text={group.sample_dir}
                className="shrink-0 text-white/40 hover:text-white p-0.5"
              />
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2 self-start sm:self-center">
          <button
            type="button"
            onClick={() => onRefreshGroupMetadata(group)}
            disabled={isRefreshing || !group.library_id}
            className="flex items-center gap-1 rounded-lg border border-white/[0.08] bg-white/[0.04] px-2.5 py-1.5 text-xs font-medium text-white/80 transition hover:bg-white/[0.08] hover:text-white disabled:opacity-50"
          >
            <SparkIcon className={`size-3.5 text-amber-300 ${isRefreshing ? "animate-pulse" : ""}`} />
            <span>{isRefreshing ? "刮削中..." : "重新刮削"}</span>
          </button>

          <button
            type="button"
            onClick={() => onToggleExpand(group)}
            className="flex items-center gap-1 rounded-lg border border-sky-500/25 bg-sky-500/10 px-2.5 py-1.5 text-xs font-medium text-sky-300 transition hover:bg-sky-500/20"
          >
            <span>{isOpen ? "收起文件" : "展开文件"}</span>
            <ChevronDownIcon
              className={`size-3.5 transition-transform ${isOpen ? "rotate-180" : ""}`}
            />
          </button>
        </div>
      </div>

      {isOpen && (
        <div className="mt-3.5 border-t border-white/[0.06] pt-3">
          {expandedState?.loading && expandedState.items.length === 0 ? (
            <div className="flex items-center gap-2 py-3 text-xs text-[var(--text-muted)]">
              <RefreshIcon className="size-3.5 animate-spin text-sky-400" />
              <span>正在加载作品文件明细...</span>
            </div>
          ) : (
            <div className="max-h-96 space-y-1.5 overflow-y-auto pr-1">
              {expandedState?.items.map((file) => {
                const epTag = formatEpisodeTag(file.units);
                const fileDur = formatItemDuration(file.duration_ms);
                const isStrm = file.is_strm || file.transfer_mode === "strm";
                const isRetransferring = retransferringId === file.id;
                const fileName = file.path.split("/").pop() || file.path;

                return (
                  <div
                    key={file.id}
                    className="flex flex-col gap-2 rounded-xl border border-white/[0.05] bg-black/25 px-3 py-2 text-xs sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
                      {epTag && (
                        <span className="shrink-0 rounded bg-sky-500/15 px-1.5 py-0.5 font-mono text-[11px] font-medium text-sky-300">
                          {epTag}
                        </span>
                      )}
                      <span
                        className="truncate font-mono text-[11px] text-white/85"
                        title={file.path}
                      >
                        {fileName}
                      </span>
                      {isStrm ? (
                        <>
                          {fileDur && (
                            <span className="shrink-0 text-[11px] text-purple-300">{fileDur}</span>
                          )}
                          <span className="shrink-0 text-[11px] text-[var(--text-faint)]">
                            云端直链
                          </span>
                        </>
                      ) : (
                        file.file_size != null &&
                        file.file_size > 0 && (
                          <span className="shrink-0 font-mono text-[11px] text-[var(--text-muted)]">
                            {formatBytes(file.file_size)}
                          </span>
                        )
                      )}
                    </div>

                    <div className="flex shrink-0 items-center gap-1.5">
                      <CopyButton
                        text={file.path}
                        className="text-white/40 hover:text-white p-1"
                      />
                      <button
                        type="button"
                        onClick={() => onRetransferFile(file)}
                        disabled={isRetransferring || isStrm || !file.source_path}
                        title={
                          isStrm
                            ? "STRM 云端直链无需重建本地硬链接"
                            : file.source_path
                              ? "重新生成文件"
                              : "缺少源下载文件路径"
                        }
                        className="rounded-md border border-sky-500/20 bg-sky-500/10 px-2 py-1 text-[11px] text-sky-300 transition hover:bg-sky-500/20 disabled:cursor-not-allowed disabled:opacity-35"
                      >
                        {isRetransferring ? "生成中" : "重建"}
                      </button>
                      <button
                        type="button"
                        onClick={() => onDeleteFile(file)}
                        disabled={deleteDisabled}
                        className="rounded-md border border-red-500/20 bg-red-500/10 p-1 text-red-400 transition hover:bg-red-500/20 disabled:opacity-40"
                        title="删除此文件记录"
                      >
                        <TrashIcon className="size-3.5" />
                      </button>
                    </div>
                  </div>
                );
              })}

              {expandedState?.hasMore && (
                <div className="pt-1.5 text-center">
                  <button
                    type="button"
                    onClick={() => onLoadMoreGroupFiles(group)}
                    disabled={expandedState.loading}
                    className="text-xs text-sky-400 hover:underline disabled:opacity-50"
                  >
                    {expandedState.loading ? "加载中..." : "加载剩余剧集文件"}
                  </button>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
