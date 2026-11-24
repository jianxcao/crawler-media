import type { TransferFileItem } from "@/lib/api/transfers";
import { formatBytes, formatVideoResolution } from "@/lib/format";
import { CopyButton } from "@/components/copy-button";
import {
  ArrowRightIcon,
  ClockIcon,
  RefreshIcon,
  SparkIcon,
  TrashIcon,
} from "@/components/icons";
import {
  formatEpisodeTag,
  formatItemDuration,
  transferModeBadgeMeta,
} from "./transfer-helpers";

interface TransferFileCardProps {
  item: TransferFileItem;
  isRetransferring: boolean;
  isRefreshing: boolean;
  deleteDisabled: boolean;
  onRetransfer: (item: TransferFileItem) => void;
  onRefreshMetadata: (item: TransferFileItem) => void;
  onDelete: (item: TransferFileItem) => void;
}

export function TransferFileCard({
  item,
  isRetransferring,
  isRefreshing,
  deleteDisabled,
  onRetransfer,
  onRefreshMetadata,
  onDelete,
}: TransferFileCardProps) {
  const epTag = formatEpisodeTag(item.units);
  const modeMeta = transferModeBadgeMeta(item.transfer_mode);
  const durationLabel = formatItemDuration(item.duration_ms);
  const isStrm = item.is_strm || item.transfer_mode === "strm";
  const retransferDisabled = isRetransferring || isStrm || !item.source_path;

  const retransferTitle = isStrm
    ? "STRM 云端直链无需重建本地硬链接"
    : item.source_path
      ? "从原始下载目录重新生成硬链接到目标媒体库"
      : "缺少源下载文件路径";

  return (
    <div className="group relative rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4 sm:p-5 transition hover:border-white/[0.14] hover:bg-white/[0.035]">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm sm:text-base font-semibold text-[var(--text)]">
              {item.media_title || "未命名作品"}
            </span>

            {epTag && (
              <span className="rounded-md border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-xs font-mono font-medium text-sky-300">
                {epTag}
              </span>
            )}

            <span className={`rounded-md px-2 py-0.5 text-xs font-medium ${modeMeta.className}`}>
              {modeMeta.label}
            </span>

            {item.resolution && (
              <span className="rounded-md bg-white/[0.06] px-2 py-0.5 text-xs font-mono text-white/80">
                {formatVideoResolution(item.resolution)}
              </span>
            )}

            {item.codec && (
              <span className="rounded-md bg-white/[0.06] px-2 py-0.5 text-xs font-mono text-white/70">
                {item.codec}
              </span>
            )}

            {item.hdr && (
              <span className="rounded-md bg-purple-500/15 border border-purple-500/25 px-1.5 py-0.5 text-[11px] font-medium text-purple-300">
                {item.hdr}
              </span>
            )}

            {isStrm ? (
              <>
                {durationLabel && (
                  <span className="inline-flex items-center gap-1 rounded-md border border-purple-500/20 bg-purple-500/10 px-2 py-0.5 text-xs font-medium text-purple-200">
                    <ClockIcon className="size-3 text-purple-300" />
                    {durationLabel}
                  </span>
                )}
                <span className="text-xs text-[var(--text-faint)]">云端直链</span>
              </>
            ) : (
              <>
                {item.file_size != null && item.file_size > 0 && (
                  <span className="text-xs text-[var(--text-muted)] font-mono">
                    {formatBytes(item.file_size)}
                  </span>
                )}
                {durationLabel && (
                  <span className="text-xs text-[var(--text-faint)]">{durationLabel}</span>
                )}
              </>
            )}
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2 self-start sm:self-center">
          <button
            type="button"
            onClick={() => onRetransfer(item)}
            disabled={retransferDisabled}
            title={retransferTitle}
            className="flex items-center gap-1 rounded-lg border border-sky-500/25 bg-sky-500/10 px-2.5 py-1.5 text-xs font-medium text-sky-300 transition hover:bg-sky-500/20 disabled:cursor-not-allowed disabled:opacity-40"
          >
            <RefreshIcon className={`size-3.5 ${isRetransferring ? "animate-spin" : ""}`} />
            <span>{isRetransferring ? "生成中..." : "重新生成文件"}</span>
          </button>

          <button
            type="button"
            onClick={() => onRefreshMetadata(item)}
            disabled={isRefreshing || !item.library_id}
            title={
              item.library_id ? "重新拉取刮削数据与元数据海报" : "无法确定该文件所属的媒体库"
            }
            className="flex items-center gap-1 rounded-lg border border-white/[0.08] bg-white/[0.04] px-2.5 py-1.5 text-xs font-medium text-white/80 transition hover:bg-white/[0.08] hover:text-white disabled:opacity-50"
          >
            <SparkIcon className={`size-3.5 text-amber-300 ${isRefreshing ? "animate-pulse" : ""}`} />
            <span>{isRefreshing ? "刮削中..." : "重新刮削"}</span>
          </button>

          <button
            type="button"
            onClick={() => onDelete(item)}
            disabled={deleteDisabled}
            className="flex items-center gap-1 rounded-lg border border-red-500/20 bg-red-500/10 px-2.5 py-1.5 text-xs font-medium text-red-400 transition hover:bg-red-500/20 hover:text-red-300 disabled:opacity-50"
          >
            <TrashIcon className="size-3.5" />
            <span>删除管理</span>
          </button>
        </div>
      </div>

      <div className="code-well mt-3.5 space-y-2 rounded-xl p-3 font-mono text-xs">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <span className="shrink-0 rounded bg-white/[0.07] px-1.5 py-0.5 font-sans text-[10px] font-medium text-white/60">
              {isStrm ? "远端源" : "下载源"}
            </span>
            <span
              className="truncate text-white/60 text-[11px]"
              title={item.source_path || (isStrm ? "云端直链指针（按需直连播放）" : "未记录下载源路径")}
            >
              {item.source_path ||
                (isStrm
                  ? "云端流媒体直链（.strm 按需直连源地址）"
                  : "未记录下载源路径（如从外部直接拷贝入库）")}
            </span>
          </div>
          {item.source_path && (
            <CopyButton
              text={item.source_path}
              className="shrink-0 text-white/40 hover:text-white p-1"
            />
          )}
        </div>

        <div className="flex items-center gap-2 pl-4 text-white/20">
          <ArrowRightIcon className="size-3 rotate-90 sm:rotate-0" />
        </div>

        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <span className="shrink-0 rounded bg-sky-500/20 px-1.5 py-0.5 font-sans text-[10px] font-semibold text-sky-300">
              入库位
            </span>
            <span
              className="truncate text-sky-300/90 text-[11px] font-medium"
              title={item.path}
            >
              {item.path}
            </span>
          </div>
          <CopyButton
            text={item.path}
            className="shrink-0 text-sky-400/60 hover:text-sky-300 p-1"
          />
        </div>
      </div>
    </div>
  );
}
