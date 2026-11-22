import type {
  TransferFilterMode,
  TransferKindFilter,
  TransferViewMode,
} from "@/lib/api/transfers";
import { LayersIcon, ListIcon, SearchIcon, XIcon } from "@/components/icons";

interface TransferToolbarProps {
  viewMode: TransferViewMode;
  searchTerm: string;
  modeFilter: TransferFilterMode;
  kindFilter: TransferKindFilter;
  filteredTotal: number;
  filteredFileTotal: number;
  onViewModeChange: (view: TransferViewMode) => void;
  onSearchTermChange: (value: string) => void;
  onClearSearch: () => void;
  onModeFilterChange: (mode: TransferFilterMode) => void;
  onKindFilterChange: (kind: TransferKindFilter) => void;
  onResetFilters: () => void;
}

const MODE_OPTIONS: Array<{
  value: TransferFilterMode;
  label: string;
  activeClass: string;
}> = [
  { value: "all", label: "全部流转", activeClass: "bg-white/15 text-white shadow-sm" },
  {
    value: "hardlink",
    label: "硬链接",
    activeClass: "bg-emerald-500/20 text-emerald-300 shadow-sm",
  },
  { value: "copy", label: "复制", activeClass: "bg-amber-500/20 text-amber-300 shadow-sm" },
  { value: "move", label: "移动", activeClass: "bg-sky-500/20 text-sky-300 shadow-sm" },
  {
    value: "strm",
    label: "STRM 直链",
    activeClass: "bg-purple-500/20 text-purple-300 shadow-sm",
  },
];

const KIND_OPTIONS: Array<{ value: TransferKindFilter; label: string }> = [
  { value: "all", label: "全部类型" },
  { value: "movie", label: "电影" },
  { value: "tv", label: "剧集" },
];

export function TransferToolbar({
  viewMode,
  searchTerm,
  modeFilter,
  kindFilter,
  filteredTotal,
  filteredFileTotal,
  onViewModeChange,
  onSearchTermChange,
  onClearSearch,
  onModeFilterChange,
  onKindFilterChange,
  onResetFilters,
}: TransferToolbarProps) {
  const hasFilters = Boolean(searchTerm || modeFilter !== "all" || kindFilter !== "all");

  return (
    <>
      <div className="mt-6 flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
        {/* 搜索框 */}
        <div className="relative flex-1 lg:max-w-md">
          <SearchIcon className="pointer-events-none absolute left-3.5 top-1/2 size-4 -translate-y-1/2 text-white/40" />
          <input
            type="text"
            value={searchTerm}
            onChange={(e) => onSearchTermChange(e.target.value)}
            placeholder="搜索作品名、文件名、源路径、编码或分辨率..."
            className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] py-2 pl-9 pr-9 text-xs sm:text-sm text-[var(--text)] placeholder-white/30 outline-none transition focus:border-white/20 focus:bg-white/[0.07]"
          />
          {searchTerm && (
            <button
              type="button"
              onClick={onClearSearch}
              aria-label="清空搜索"
              className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded-md p-1 text-white/40 hover:text-white"
            >
              <XIcon className="size-3.5" />
            </button>
          )}
        </div>

        {/* 视图切换 + 过滤 Pills */}
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5 text-xs">
            <button
              type="button"
              onClick={() => onViewModeChange("files")}
              className={`inline-flex items-center gap-1 rounded-lg px-2.5 py-1 font-medium transition ${
                viewMode === "files"
                  ? "bg-white/15 text-white shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              <ListIcon className="size-3.5" />
              <span>逐文件列表</span>
            </button>
            <button
              type="button"
              onClick={() => onViewModeChange("groups")}
              className={`inline-flex items-center gap-1 rounded-lg px-2.5 py-1 font-medium transition ${
                viewMode === "groups"
                  ? "bg-sky-500/20 text-sky-300 shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              <LayersIcon className="size-3.5" />
              <span>按作品聚合</span>
            </button>
          </div>

          <div className="flex flex-wrap rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5 text-xs">
            {MODE_OPTIONS.map((opt) => (
              <button
                key={opt.value}
                type="button"
                onClick={() => onModeFilterChange(opt.value)}
                className={`rounded-lg px-2.5 py-1 font-medium transition ${
                  modeFilter === opt.value ? opt.activeClass : "text-white/60 hover:text-white"
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>

          <div className="flex rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5 text-xs">
            {KIND_OPTIONS.map((opt) => (
              <button
                key={opt.value}
                type="button"
                onClick={() => onKindFilterChange(opt.value)}
                className={`rounded-lg px-2.5 py-1 font-medium transition ${
                  kindFilter === opt.value
                    ? "bg-white/15 text-white shadow-sm"
                    : "text-white/60 hover:text-white"
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div className="mt-3 flex items-center justify-between text-xs text-[var(--text-muted)]">
        <span>
          {viewMode === "groups" ? (
            <>
              共匹配{" "}
              <strong className="font-semibold text-[var(--text)]">
                {filteredTotal.toLocaleString("zh-CN")}
              </strong>{" "}
              部作品（含{" "}
              <strong className="font-semibold text-[var(--text)]">
                {filteredFileTotal.toLocaleString("zh-CN")}
              </strong>{" "}
              个文件）
            </>
          ) : (
            <>
              共找到{" "}
              <strong className="font-semibold text-[var(--text)]">
                {filteredTotal.toLocaleString("zh-CN")}
              </strong>{" "}
              条入库记录
            </>
          )}
        </span>

        {hasFilters && (
          <button
            type="button"
            onClick={onResetFilters}
            className="text-sky-400 hover:underline"
          >
            重置所有筛选
          </button>
        )}
      </div>
    </>
  );
}
