"use client";

import { PlusIcon, SearchIcon, XIcon } from "@/components/icons";
import { FilterChip } from "@/components/library-manage/reorder-dialog";
import type { MediaLibrary } from "@/lib/api/libraries";
import type { LibraryFilter, LibraryFocus, LibrarySummary } from "@/lib/library-manage";
import { KIND_ORDER } from "@/lib/library-manage-helpers";
import { LIBRARY_KIND_LABELS, type LibraryKind } from "@/lib/media-types";

/** 媒体库管理页的页头摘要、子标签栏与筛选工具栏。 */
export function LibraryManageHeader({
  libraries,
  summary,
  filter,
  tab,
  onCreate,
  onToggleFocus,
  onTabChange,
}: {
  libraries: MediaLibrary[] | null;
  summary: LibrarySummary;
  filter: LibraryFilter;
  tab: "libraries" | "duplicates";
  onCreate: () => void;
  onToggleFocus: (focus: LibraryFocus) => void;
  onTabChange: (tab: "libraries" | "duplicates") => void;
}) {
  return (
    <>
      <div className="px-6 pt-3 max-md:px-4">
        <div className="flex items-start justify-between gap-4">
          <h2 className="text-on-image text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            媒体库管理
          </h2>
          <button
            type="button"
            onClick={onCreate}
            className="btn-accent mt-1 flex h-9 shrink-0 items-center gap-1 rounded-full py-0 pl-3 pr-4 text-ui font-semibold max-md:mt-0"
          >
            <PlusIcon className="size-4" />
            创建媒体库
          </button>
        </div>
        <div className="text-on-image mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-ui text-[var(--text-muted)] max-md:text-sub">
          <span>
            {libraries === null
              ? "正在汇总媒体库…"
              : libraries.length === 0
                ? "还没有媒体库"
                : summary.facts}
          </span>
          {summary.busy > 0 && (
            <FilterChip active={filter.focus === "busy"} onClick={() => onToggleFocus("busy")}>
              <span className="size-1.5 rounded-full bg-[var(--info)]" />
              {summary.busy} 个在跑任务
            </FilterChip>
          )}
          {summary.attention > 0 && (
            <FilterChip
              active={filter.focus === "attention"}
              onClick={() => onToggleFocus("attention")}
            >
              <span
                className={`size-1.5 rounded-full ${summary.missing ? "bg-[var(--danger)]" : "bg-[var(--warn)]"}`}
              />
              {summary.attention} 个库有待处理文件
            </FilterChip>
          )}
          {libraries !== null &&
            libraries.length > 0 &&
            summary.busy === 0 &&
            summary.attention === 0 && <span className="text-[var(--text-faint)]">一切正常</span>}
        </div>
      </div>

      <div className="mt-5 flex flex-wrap gap-1.5 px-6 max-md:px-4">
        {(
          [
            ["libraries", "媒体库"],
            ["duplicates", "重复文件"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={tab === id}
            onClick={() => onTabChange(id)}
            className={`rounded-full px-3.5 py-1.5 text-ui font-medium transition ${
              tab === id
                ? "bg-white/[0.14] text-white"
                : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-white"
            }`}
          >
            {label}
          </button>
        ))}
      </div>
    </>
  );
}

/** 媒体库列表的关键词搜索与类型筛选栏。 */
export function LibraryManageFilterToolbar({
  totalLibraries,
  kindCounts,
  filter,
  onFilterChange,
}: {
  totalLibraries: number;
  kindCounts: Map<LibraryKind, number>;
  filter: LibraryFilter;
  onFilterChange: React.Dispatch<React.SetStateAction<LibraryFilter>>;
}) {
  return (
    <div className="mt-5 flex flex-wrap items-center gap-2.5 px-6 max-md:px-4">
      <label className="flex h-9 min-w-[220px] flex-1 items-center gap-2 rounded-full border border-white/[0.08] bg-white/[0.04] px-3 text-ui text-[var(--text-muted)] focus-within:border-[var(--accent)]/60 max-md:min-w-0 max-md:basis-full sm:max-w-[320px]">
        <SearchIcon className="size-4 shrink-0" />
        <input
          type="search"
          value={filter.query}
          onChange={(e) => onFilterChange((f) => ({ ...f, query: e.target.value }))}
          placeholder="按库名或根目录搜索"
          aria-label="搜索媒体库"
          className="min-w-0 flex-1 bg-transparent text-[var(--text)] outline-none placeholder:text-[var(--text-faint)]"
        />
        {filter.query && (
          <button
            type="button"
            aria-label="清除搜索"
            onClick={() => onFilterChange((f) => ({ ...f, query: "" }))}
            className="grid size-5 place-items-center rounded-full hover:bg-white/[0.1]"
          >
            <XIcon className="size-3" />
          </button>
        )}
      </label>
      <div className="flex flex-wrap items-center gap-1.5">
        <FilterChip
          active={filter.kind === null}
          onClick={() => onFilterChange((f) => ({ ...f, kind: null }))}
        >
          全部 {totalLibraries}
        </FilterChip>
        {KIND_ORDER.filter((k) => (kindCounts.get(k) ?? 0) > 0).map((k) => (
          <FilterChip
            key={k}
            active={filter.kind === k}
            onClick={() =>
              onFilterChange((f) => ({ ...f, kind: f.kind === k ? null : k }))
            }
          >
            {LIBRARY_KIND_LABELS[k]} {kindCounts.get(k)}
          </FilterChip>
        ))}
      </div>
    </div>
  );
}
