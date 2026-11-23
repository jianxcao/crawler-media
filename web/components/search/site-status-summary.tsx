"use client";

import { useState } from "react";
import { Link } from "react-router-dom";

import { formatRelativeTime } from "@/lib/time";
import { formatElapsed } from "@/lib/torrent-search-view";

/** 单个站点的实时进度（由 start / site_result / site_error 事件逐步填充）。 */
export interface SiteProgress {
  site_id: string;
  site_name: string;
  phase: "searching" | "ok" | "error";
  count: number;
  error: string | null;
  elapsed_ms: number | null;
}

/** 站点状态聚合 chip + 逐站详情弹层。 */
export function SiteStatusSummary({
  sites,
  streaming,
  totalElapsedMs,
  onRetrySite,
}: {
  sites: SiteProgress[];
  streaming: boolean;
  totalElapsedMs: number | null;
  onRetrySite?: (siteId: string) => void;
}) {
  const [open, setOpen] = useState(false);

  if (sites.length === 0) return null;

  const settled = sites.filter((s) => s.phase !== "searching").length;
  const failed = sites.filter((s) => s.phase === "error").length;
  const dotCls = streaming
    ? "animate-pulse bg-[var(--accent)]"
    : failed > 0
      ? "bg-[#ff6b6b]"
      : "bg-[var(--ok)]";

  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        title="查看各站点的搜索详情"
        className="flex items-center gap-1.5 rounded-full border border-white/[0.12] bg-white/[0.05] px-2.5 py-1 text-caption text-[var(--text-muted)] backdrop-blur-sm transition-colors hover:border-white/[0.22] hover:text-[var(--text)]"
      >
        <span className={`size-1.5 rounded-full ${dotCls}`} />
        {streaming ? (
          <span className="tnum">{settled}/{sites.length} 站点</span>
        ) : (
          <>
            <span className="tnum">{sites.length} 站点</span>
            {failed > 0 && (
              <span className="tnum text-[var(--danger)]">· {failed} 失败</span>
            )}
          </>
        )}
        <span className="text-micro opacity-70">▾</span>
      </button>

      {open && (
        <>
          <div className="fixed inset-0 z-20" onClick={() => setOpen(false)} />
          <div className="popover-surface absolute right-0 top-full z-30 mt-2 w-[320px] max-w-[82vw] rounded-2xl p-2">
            <ul className="flex flex-col">
              {sites.map((s) => (
                <li key={s.site_id} className="rounded-lg px-2 py-1.5">
                  <div className="flex items-center gap-2">
                    <span
                      className={`size-1.5 shrink-0 rounded-full ${
                        s.phase === "searching"
                          ? "animate-pulse bg-[var(--accent)]"
                          : s.phase === "error"
                            ? "bg-[var(--danger)]"
                            : "bg-[var(--ok)]"
                      }`}
                    />
                    <span className="min-w-0 flex-1 truncate text-sub text-[var(--text)]">
                      {s.site_name}
                    </span>
                    {s.phase === "searching" && (
                      <span className="shrink-0 text-caption text-[var(--text-faint)]">
                        搜索中…
                      </span>
                    )}
                    {s.phase === "ok" && (
                      <span className="tnum shrink-0 text-caption text-[var(--text-muted)]">
                        {s.count} 条
                        {s.elapsed_ms !== null && (
                          <span className="text-[var(--text-faint)]">
                            {" "}· {formatElapsed(s.elapsed_ms)}
                          </span>
                        )}
                      </span>
                    )}
                    {s.phase === "error" && (
                      <span className="tnum shrink-0 text-caption text-[var(--danger)]">
                        失败
                        {s.elapsed_ms !== null && (
                          <span className="opacity-75"> · {formatElapsed(s.elapsed_ms)}</span>
                        )}
                      </span>
                    )}
                  </div>
                  {s.phase === "error" && s.error && (
                    <p
                      title={s.error}
                      className="mt-0.5 line-clamp-2 pl-3.5 text-caption leading-4 text-[var(--danger)]/85"
                    >
                      {s.error}
                    </p>
                  )}
                  {s.phase === "error" && (
                    <div className="mt-1 flex gap-2 pl-3.5">
                      {onRetrySite && (
                        <button
                          type="button"
                          onClick={() => onRetrySite(s.site_id)}
                          className="rounded-md bg-white/[0.08] px-2 py-0.5 text-caption font-medium text-white/80 transition hover:bg-white/[0.15]"
                        >
                          重试该站
                        </button>
                      )}
                      <Link
                        to="/settings/sites"
                        className="rounded-md bg-white/[0.08] px-2 py-0.5 text-caption font-medium text-white/80 transition hover:bg-white/[0.15]"
                      >
                        去站点设置 ›
                      </Link>
                    </div>
                  )}
                </li>
              ))}
            </ul>
            {totalElapsedMs !== null && (
              <div className="mt-1 border-t border-white/[0.08] px-2 pb-0.5 pt-1.5 text-caption text-[var(--text-faint)]">
                总耗时 {formatElapsed(totalElapsedMs)}
                <span className="opacity-70">（以最慢的站点为准）</span>
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}

/** 页头底部的搜索进度条。 */
export function SearchProgressBar({ settled, total }: { settled: number; total: number }) {
  return (
    <div className="relative mt-3 h-[2px] overflow-hidden rounded-full bg-white/[0.07]">
      {total === 0 ? (
        <span className="progress-sweep absolute inset-y-0 w-2/5 rounded-full bg-gradient-to-r from-transparent via-[var(--accent)] to-transparent" />
      ) : (
        <span
          className="absolute inset-y-0 left-0 rounded-full bg-gradient-to-r from-[var(--accent)] to-[var(--accent-2)] transition-[width] duration-500 ease-out"
          style={{ width: `${Math.max(6, (settled / total) * 100)}%` }}
        />
      )}
    </div>
  );
}

const SKELETON_TITLE_WIDTHS = ["72%", "58%", "80%", "64%", "70%", "52%"];

/** 首批结果到达前的等待骨架屏。 */
export function SkeletonList({ siteCount }: { siteCount: number }) {
  return (
    <div>
      <div className="text-on-image flex items-center gap-2 px-1 pb-3 text-sub text-[var(--text-muted)]">
        <svg
          viewBox="0 0 24 24"
          className="size-3.5 animate-spin text-[var(--accent)]"
          fill="none"
          stroke="currentColor"
          strokeWidth={2.5}
          strokeLinecap="round"
          aria-hidden="true"
        >
          <path d="M12 3a9 9 0 1 1-9 9" />
        </svg>
        {siteCount > 0
          ? `正在搜索 ${siteCount} 个站点，结果将实时呈现…`
          : "正在连接搜索服务…"}
      </div>
      <ul className="space-y-2" aria-hidden="true">
        {SKELETON_TITLE_WIDTHS.map((width, i) => (
          <li
            key={i}
            className="css-glass rounded-2xl px-4 py-3"
            style={{ opacity: 1 - i * 0.13, "--stagger": `${i * 120}ms` } as React.CSSProperties}
          >
            <div className="flex items-center gap-1.5">
              <span className="skeleton-block h-[17px] w-14 rounded-md" />
              <span className="skeleton-block h-[17px] w-10 rounded-md" />
              <span className="skeleton-block h-[17px] w-12 rounded-md" />
            </div>
            <div className="skeleton-block mt-2.5 h-[13px] rounded-md" style={{ width }} />
            <div className="mt-2.5 flex items-center gap-3">
              <span className="skeleton-block h-[10px] w-14 rounded" />
              <span className="skeleton-block h-[10px] w-9 rounded" />
              <span className="skeleton-block h-[10px] w-9 rounded" />
              <span className="skeleton-block h-[10px] w-16 rounded" />
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function EmptyHint({ title, hint }: { title: string; hint: string }) {
  return (
    <div className="flex min-h-[240px] flex-col items-center justify-center text-center">
      <p className="text-on-image text-body-lg font-medium text-white">{title}</p>
      <p className="text-on-image mt-1.5 max-w-sm text-ui leading-6 text-[var(--text-muted)]">
        {hint}
      </p>
    </div>
  );
}

/** 历史结果快照年龄标签与「重新搜索」按钮。 */
export function SnapshotBadge({
  snapshotAt,
  onResearch,
}: {
  snapshotAt: string;
  onResearch?: () => void;
}) {
  return (
    <>
      <span
        title="这是历史留存的结果快照，站点数据（做种数/促销/链接）可能已变化"
        className="flex items-center gap-1.5 rounded-full border border-[var(--info)]/30 bg-[var(--info)]/10 px-2.5 py-1 text-caption text-[var(--info)] backdrop-blur-sm"
      >
        <svg
          viewBox="0 0 24 24"
          className="size-[13px] shrink-0"
          fill="none"
          stroke="currentColor"
          strokeWidth={1.8}
          strokeLinecap="round"
          aria-hidden="true"
        >
          <circle cx="12" cy="12" r="8.5" />
          <path d="M12 7.5V12l3 2" />
        </svg>
        {formatRelativeTime(snapshotAt)}的快照
      </span>
      {onResearch && (
        <button
          type="button"
          onClick={onResearch}
          className="btn-accent rounded-full px-2.5 py-1 text-caption font-medium"
        >
          重新搜索
        </button>
      )}
    </>
  );
}
