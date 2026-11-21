"use client";

import type { ReactNode } from "react";

import type { MediaLibrary, MissingItem, UnidentifiedGroup } from "@/lib/api/libraries";
import { formatBytes } from "@/lib/format";

export function LibraryDetailHeader({
  library,
  kindLabel,
  canManageLibraries,
  removalActive,
  removalNotice,
  missing,
  missingFileCount,
  unidentified,
  unidentifiedFiles,
  failed,
  notice,
  onOpenMissing,
  onClearMissingFiles,
  onOpenUnidentified,
  onRescan,
  onRemoveLibrary,
}: {
  library: MediaLibrary;
  kindLabel: string;
  canManageLibraries: boolean;
  removalActive: boolean;
  removalNotice: ReactNode;
  missing: MissingItem[];
  missingFileCount: number;
  unidentified: UnidentifiedGroup[];
  unidentifiedFiles: number;
  failed: boolean;
  notice: string | null;
  onOpenMissing: () => void;
  onClearMissingFiles: () => void;
  onOpenUnidentified: () => void;
  onRescan: () => void;
  onRemoveLibrary: () => void;
}) {
  const { stats } = library;

  return (
    <div className="px-6 max-md:px-4">
      <div className="flex items-center gap-2.5">
        <h2 className="text-on-image truncate text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[20px]">
          {library.name}
        </h2>
        {library.is_default && (
          <span className="shrink-0 rounded-full border border-white/[0.14] bg-white/[0.12] px-2 py-0.5 text-caption font-semibold text-white/90">
            默认
          </span>
        )}
      </div>
      <p className="text-on-image mt-1.5 truncate text-ui text-[var(--text-muted)] max-md:text-sub">
        {kindLabel}库 · {stats.item_count} 部作品 ·{" "}
        {stats.file_count} 个文件
        {(() => {
          const strmCount = stats.strm_file_count ?? 0;
          const hasLocalBytes = stats.total_size_bytes > 0;
          if (hasLocalBytes && strmCount > 0) {
            return ` · 本地 ${formatBytes(stats.total_size_bytes)} · ${strmCount} 个云端`;
          }
          if (hasLocalBytes) {
            return ` · ${formatBytes(stats.total_size_bytes)}`;
          }
          if (strmCount > 0) {
            return ` · ${strmCount} 个云挂载`;
          }
          return " · 流媒体/云挂载";
        })()}
      </p>
      {removalNotice}
      {/* —— 根目录缺失：目录被删/未挂载不能当「空库」展示，给恢复/删除两条路 —— */}
      {library.root_missing && canManageLibraries && !removalActive && (
        <div className="mt-3 rounded-xl border border-red-400/30 bg-red-500/[0.08] px-3.5 py-3">
          <p className="text-sub font-semibold text-red-200">
            媒体库目录不存在（可能被删除或存储未挂载）
          </p>
          <p className="mt-1 text-caption text-[var(--text-muted)]">
            {library.root_paths.join("、")}
          </p>
          <div className="mt-2.5 flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={onRescan}
              className="btn-glass h-7 shrink-0 px-2.5 text-caption font-medium"
            >
              重新扫描（目录恢复后找回内容）
            </button>
            <button
              type="button"
              onClick={onRemoveLibrary}
              className="h-7 shrink-0 rounded-lg border border-red-400/40 px-2.5 text-caption font-medium text-red-200 transition hover:bg-red-500/20"
            >
              删除媒体库
            </button>
          </div>
        </div>
      )}
      {/* —— 健康状态胶囊：缺失 / 待识别 / 待复核 收进抽屉，海报墙保持干净 —— */}
      {canManageLibraries && (missing.length > 0 || unidentified.length > 0) && (
        <div className="mt-2.5 flex flex-wrap items-center gap-2">
          {missing.length > 0 && (
            <button
              type="button"
              onClick={onOpenMissing}
              className="flex items-center gap-1.5 rounded-full border border-white/[0.14] bg-white/[0.06] px-3 py-1 text-sub font-semibold text-white/75 transition hover:bg-white/[0.12] hover:text-white"
            >
              <span className="size-1.5 rounded-full bg-white/40" />
              {missing.length} 个条目缺失
            </button>
          )}
          {missingFileCount > 0 && (
            <button
              type="button"
              onClick={onClearMissingFiles}
              className="flex items-center gap-1.5 rounded-full border border-red-400/40 bg-red-500/10 px-3 py-1 text-sub font-semibold text-red-200 transition hover:bg-red-500/20"
            >
              <span className="size-1.5 rounded-full bg-red-400" />
              {missingFileCount} 个文件缺失 · 清理
            </button>
          )}
          {unidentified.length > 0 && (
            <button
              type="button"
              onClick={onOpenUnidentified}
              className="flex items-center gap-1.5 rounded-full border border-[var(--warn)]/35 bg-[var(--warn)]/[0.12] px-3 py-1 text-sub font-semibold text-[var(--warn)] transition hover:bg-[var(--warn)]/[0.22]"
            >
              <span className="size-1.5 rounded-full bg-[var(--warn)]" />
              {unidentifiedFiles} 个文件待识别
              {unidentified.length > 1 ? ` · ${unidentified.length} 组` : ""}
            </button>
          )}
        </div>
      )}
      {failed && (
        <p className="mt-3 rounded-lg border border-amber-400/25 bg-amber-500/10 px-3.5 py-2 text-sub text-amber-200">
          与后端通信失败，正在自动重试；下方显示的是最近一次成功加载的数据
        </p>
      )}
      {notice && (
        <p className="mt-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2 text-sub text-red-200">
          {notice}
        </p>
      )}
    </div>
  );
}
