import type { TransferSummary } from "@/lib/api/transfers";
import { formatBytes } from "@/lib/format";
import { FilmIcon, TvIcon } from "@/components/icons";
import { formatSummaryDuration } from "./transfer-helpers";

interface TransferSummaryCardsProps {
  summary: TransferSummary;
}

export function TransferSummaryCards({ summary }: TransferSummaryCardsProps) {
  const total = summary.total_files;
  const zeroOverheadCount = summary.hardlink_count + summary.strm_count;
  const zeroOverheadRate =
    total > 0 ? Math.min(100, Math.round((zeroOverheadCount / total) * 100)) : 0;
  const isPureStrm = summary.local_size_bytes === 0 && summary.strm_count > 0;

  return (
    <div className="mt-6 grid grid-cols-1 gap-3 sm:grid-cols-3">
      {/* 指标 1: 已整理资产 */}
      <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
        <div className="text-xs font-medium text-[var(--text-muted)]">已整理资产</div>
        <div className="mt-2 flex flex-wrap items-baseline gap-2">
          <span className="text-2xl sm:text-3xl font-bold tracking-tight text-[var(--text)]">
            {total.toLocaleString("zh-CN")}
          </span>
          <span className="text-xs text-[var(--text-faint)]">
            项文件（共 {summary.media_count.toLocaleString("zh-CN")} 部作品）
          </span>
        </div>
        <div className="mt-2.5 flex items-center gap-2 text-micro text-[var(--text-muted)]">
          <span className="inline-flex items-center gap-1 text-[var(--text-faint)]">
            <FilmIcon className="size-3" />
            {summary.movie_files.toLocaleString("zh-CN")} 电影
          </span>
          <span className="text-white/20">·</span>
          <span className="inline-flex items-center gap-1 text-[var(--text-faint)]">
            <TvIcon className="size-3" />
            {summary.tv_files.toLocaleString("zh-CN")} 剧集
          </span>
        </div>
      </div>

      {/* 指标 2: 零盘占流转构成 */}
      <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
        <div className="flex items-center justify-between gap-2">
          <span className="text-xs font-medium text-[var(--text-muted)]">零盘占流转构成</span>
          <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-medium text-emerald-400">
            零额外本地视频冗余
          </span>
        </div>
        <div className="mt-2 flex flex-wrap items-baseline gap-2">
          <span className="text-2xl sm:text-3xl font-bold tracking-tight text-emerald-400">
            {zeroOverheadCount.toLocaleString("zh-CN")}
          </span>
          <span className="text-xs text-emerald-400/80">
            / {total.toLocaleString("zh-CN")} ({zeroOverheadRate}%)
          </span>
        </div>
        <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-white/[0.06]">
          <div
            className="h-full rounded-full bg-gradient-to-r from-emerald-500 to-purple-400 transition-all duration-500"
            style={{ width: `${zeroOverheadRate}%` }}
          />
        </div>
        <div className="mt-2 text-micro text-[var(--text-muted)]">
          STRM 直链 {summary.strm_count.toLocaleString("zh-CN")} · 硬链接{" "}
          {summary.hardlink_count.toLocaleString("zh-CN")}
        </div>
      </div>

      {/* 指标 3: 存储占用与片长 */}
      <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
        {isPureStrm ? (
          <>
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs font-medium text-[var(--text-muted)]">云端直链总片长</span>
              <span className="inline-flex items-center gap-1 rounded-full bg-purple-500/15 px-2 py-0.5 text-[11px] font-medium text-purple-300">
                本地零视频占用
              </span>
            </div>
            <div className="mt-2 flex items-baseline gap-2">
              <span className="text-2xl sm:text-3xl font-bold tracking-tight text-purple-300">
                {formatSummaryDuration(summary.probed_duration_ms)}
              </span>
            </div>
            <div className="mt-2.5 text-micro text-[var(--text-muted)]">
              共 {summary.strm_count.toLocaleString("zh-CN")} 项 STRM 直链 · 已探测时长{" "}
              {summary.strm_probed_count.toLocaleString("zh-CN")} 项
            </div>
          </>
        ) : (
          <>
            <div className="text-xs font-medium text-[var(--text-muted)]">本地归档视频体积</div>
            <div className="mt-2 flex items-baseline gap-2">
              <span className="text-2xl sm:text-3xl font-bold tracking-tight text-sky-400">
                {formatBytes(summary.local_size_bytes)}
              </span>
            </div>
            <div className="mt-2.5 text-micro text-[var(--text-muted)]">
              {summary.strm_count > 0
                ? `另含 ${summary.strm_count.toLocaleString("zh-CN")} 项 STRM 云端直链（不占本地空间）`
                : "源下载目录与入库目录双向追踪"}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
