"use client";

import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import {
  type FacetValue,
  filterCount,
  getLibraryRelax,
  type LibraryFacets,
  type LibraryFilter,
  type LibraryRelax,
} from "@/lib/api/libraries";
import {
  buildFilterConditionGroups,
  dropFilterValue,
  facetLabelOf,
} from "@/lib/library-filter-helpers";
import { usePermissions } from "@/lib/permissions";

export function ConditionRow({
  filter,
  facets,
  onFilterChange,
  onClear,
}: {
  filter: LibraryFilter;
  facets: LibraryFacets | null;
  onFilterChange: (next: LibraryFilter) => void;
  onClear: () => void;
}) {
  const groups = buildFilterConditionGroups(filter);

  return (
    <div className="mt-2.5 flex flex-wrap items-center gap-2">
      {groups.map((group, index) => (
        <div key={group.key} className="flex items-center gap-2">
          {/* 维度之间是「且」——语义画出来，不写在脚注里 */}
          {index > 0 && <span className="text-caption tracking-wide text-white/30">且</span>}
          <span className="glass-row flex h-7 !w-auto items-center gap-1.5 rounded-lg !bg-[var(--glass-fill-active)] !px-2 py-0">
            <span className="rounded bg-black/25 px-1.5 py-0.5 text-caption text-white/40">
              {group.label}
            </span>
            {group.values.map((value, i) => {
              const display = facetLabelOf(group.key, value, facets);
              return (
                <span key={value} className="flex items-center gap-1.5">
                  {/* 同一维度内的多个值是「或」 */}
                  {i > 0 && <span className="text-caption text-white/30">或</span>}
                  <span className="flex items-center gap-0.5 text-caption font-semibold text-white">
                    {display}
                    <button
                      type="button"
                      aria-label={`取消 ${group.label} ${display}`}
                      onClick={() => onFilterChange(dropFilterValue(filter, group.key, value))}
                      className="rounded-full px-1 text-white/40 hover:bg-white/15 hover:text-white"
                    >
                      ✕
                    </button>
                  </span>
                </span>
              );
            })}
          </span>
        </div>
      ))}
      <button
        type="button"
        onClick={onClear}
        className="rounded-full px-2.5 py-1 text-caption text-white/50 hover:bg-white/10 hover:text-white"
      >
        清空
      </button>
      {facets && (
        <span className="ml-auto text-caption tabular-nums text-white/60">
          筛出 <span className="font-mono font-semibold text-white">{facets.total}</span> 部
        </span>
      )}
    </div>
  );
}

/** 一组固定档位的胶囊。0 的置灰不可点——「永不空货架」的第一道闸。 */
export function PillGroup({
  label,
  hint,
  options,
  selected,
  onToggle,
  tone,
}: {
  label: string;
  hint?: string;
  options: readonly FacetValue[];
  selected: readonly string[];
  onToggle: (value: string) => void;
  /** 语义色：库存状态里「要处理的事」和「不用处理的事」得一眼分得开 */
  tone?: Record<string, string>;
}) {
  if (options.length === 0) return null;
  return (
    <div className="mb-3.5">
      <p className="mb-1.5 flex items-baseline gap-1.5 text-caption text-[var(--text-faint)]">
        {label}
        {hint && <span className="text-white/25">{hint}</span>}
      </p>
      <div className="flex flex-wrap gap-1.5">
        {options.map((option) => {
          const on = selected.includes(option.value);
          const dead = option.count === 0 && !on;
          return (
            <button
              key={option.value}
              type="button"
              disabled={dead}
              onClick={() => onToggle(option.value)}
              // 窄屏加高：这里是抽屉里唯一的点按目标，py-0.5 的胶囊拇指很难点准
              className={`rounded-full border px-2.5 py-0.5 text-caption transition-colors disabled:pointer-events-none disabled:opacity-30 max-md:px-3 max-md:py-1.5 ${
                on
                  ? "border-white/40 bg-white/[0.14] font-semibold text-white"
                  : `border-white/[0.14] hover:bg-white/[0.08] ${tone?.[option.value] ?? "text-white/65"}`
              }`}
            >
              {option.label}
              <span className="ml-1.5 font-mono text-[10px] tabular-nums text-white/35">
                {option.count}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

/**
 * 「更多筛选」双栏面板：左「找片」、右「查库」。
 *
 * 分栏依据是作用对象不同：左栏问「作品是什么样的」（来自刮削档案），
 * 右栏问「文件是什么规格」（来自库存台账）。
 */
export function MoreFiltersPanel({
  facets,
  loading,
  filter,
  onFilterChange,
}: {
  facets: LibraryFacets | null;
  /** 全份 facet 还没到：先给一句话，别摆两个空标题等着内容弹进来 */
  loading?: boolean;
  filter: LibraryFilter;
  onFilterChange: (next: LibraryFilter) => void;
}) {
  const toggleList = (key: "runtimes" | "languages" | "resolutions" | "stock", value: string) => {
    const current = filter[key] ?? [];
    onFilterChange({
      ...filter,
      [key]: current.includes(value) ? current.filter((v) => v !== value) : [...current, value],
    });
  };

  if (loading) {
    return (
      <div className="mt-2.5 rounded-2xl border border-white/[0.1] bg-black/20 px-4 py-6 text-center text-sub text-[var(--text-faint)] max-md:mt-0 max-md:rounded-none max-md:border-x-0 max-md:border-b-0 max-md:border-white/[0.08] max-md:bg-transparent max-md:px-0 max-md:py-4">
        正在数各档位还剩多少部…
      </div>
    );
  }
  return (
    <div className="mt-2.5 grid grid-cols-2 gap-x-6 rounded-2xl border border-white/[0.1] bg-black/20 p-4 max-md:mt-0 max-md:grid-cols-1 max-md:rounded-none max-md:border-0 max-md:bg-transparent max-md:p-0">
      <div className="max-md:border-t max-md:border-white/[0.08] max-md:pt-3">
        <p className="mb-3 border-b border-white/[0.08] pb-2 text-sub font-semibold text-white max-md:mb-2.5 max-md:border-b-0 max-md:pb-0">
          找片
          <span className="ml-2 text-caption font-normal text-[var(--text-faint)]">
            作品是什么样的 · 来自刮削档案
          </span>
        </p>
        <PillGroup
          label="评分"
          options={facets?.ratings ?? []}
          selected={filter.ratingGte != null ? [String(filter.ratingGte)] : []}
          onToggle={(v) =>
            onFilterChange({
              ...filter,
              // 评分是阈值不是集合：再点一次同一档就是取消
              ratingGte: filter.ratingGte === Number(v) ? null : Number(v),
            })
          }
        />
        <PillGroup
          label="片长"
          options={facets?.runtimes ?? []}
          selected={filter.runtimes ?? []}
          onToggle={(v) => toggleList("runtimes", v)}
        />
        <PillGroup
          label="原始语言"
          options={facets?.languages ?? []}
          selected={filter.languages ?? []}
          onToggle={(v) => toggleList("languages", v)}
        />
      </div>
      <div className="border-l border-white/[0.08] pl-6 max-md:border-l-0 max-md:border-t max-md:pl-0 max-md:pt-3">
        <p className="mb-3 border-b border-white/[0.08] pb-2 text-sub font-semibold text-white max-md:mb-2.5 max-md:border-b-0 max-md:pb-0">
          查库
          <span className="ml-2 text-caption font-normal text-[var(--text-faint)]">
            文件是什么规格 · 来自库存台账
          </span>
        </p>
        <PillGroup
          label="分辨率"
          options={facets?.resolutions ?? []}
          selected={filter.resolutions ?? []}
          onToggle={(v) => toggleList("resolutions", v)}
        />
        <PillGroup
          label="动态范围"
          options={facets?.hdr ?? []}
          selected={filter.hdr == null ? [] : [filter.hdr ? "1" : "0"]}
          onToggle={(v) => {
            const next = v === "1";
            onFilterChange({ ...filter, hdr: filter.hdr === next ? null : next });
          }}
        />
        <PillGroup
          label="库存状态"
          hint="要处理的事"
          options={facets?.stock ?? []}
          selected={filter.stock ?? []}
          onToggle={(v) => toggleList("stock", v)}
          tone={{ missing: "text-[var(--danger)]", unscraped: "text-[var(--warn)]" }}
        />
      </div>
    </div>
  );
}

/**
 * 窄屏的筛选抽屉：点「筛选」直接拉起，一二级维度全在里面。
 */
export function FilterSheet({
  facets,
  loading,
  filter,
  onFilterChange,
  onToggle,
  onToggleWatch,
  onClose,
}: {
  facets: LibraryFacets | null;
  /** 全份 facet 还没到（二级维度的档位要等它） */
  loading?: boolean;
  filter: LibraryFilter;
  onFilterChange: (next: LibraryFilter) => void;
  onToggle: (dim: "genres" | "countries" | "decades", value: string) => void;
  onToggleWatch: (value: string) => void;
  onClose: () => void;
}) {
  // Esc / 点上方留白都收起（与全站弹层一致）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  if (typeof document === "undefined") return null;
  return createPortal(
    <div className="fixed inset-x-0 top-0 z-50 flex flex-col justify-end [bottom:calc(-1*var(--vp-overshoot))]">
      {/* 上半截只压一层很淡的幕：压黑了就等于全屏，看不见墙在变 */}
      <button
        type="button"
        aria-label="收起筛选"
        onClick={onClose}
        className="flex-1 cursor-default bg-black/25"
      />
      {/* 装下一二级全部维度，比原先只装二级时高一些，但仍给上方留出一截墙 */}
      <div className="flex max-h-[70dvh] flex-col rounded-t-2xl border-t border-white/10 bg-[rgba(16,18,26,0.92)] shadow-[0_-12px_40px_rgba(0,0,0,0.5)] backdrop-blur-2xl">
        <div className="flex shrink-0 items-center justify-center py-2">
          <span className="h-1 w-9 rounded-full bg-white/25" />
        </div>
        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-4">
          {/* 一级四维：与宽屏那排下拉同一份取值与计数，只是换成平铺 */}
          <PillGroup
            label="类型"
            hint="可多选 · 维度内是「或」"
            options={facets?.genres ?? []}
            selected={(filter.genres ?? []).map(String)}
            onToggle={(v) => onToggle("genres", v)}
          />
          <PillGroup
            label="年代"
            options={facets?.decades ?? []}
            selected={filter.decades ?? []}
            onToggle={(v) => onToggle("decades", v)}
          />
          <PillGroup
            label="地区"
            hint="可多选 · 维度内是「或」"
            options={facets?.countries ?? []}
            selected={filter.countries ?? []}
            onToggle={(v) => onToggle("countries", v)}
          />
          <PillGroup
            label="观看"
            hint="单选"
            options={facets?.watch ?? []}
            selected={filter.watch ? [filter.watch] : []}
            onToggle={onToggleWatch}
          />
          <MoreFiltersPanel
            facets={facets}
            loading={loading}
            filter={filter}
            onFilterChange={onFilterChange}
          />
        </div>
        <div className="flex shrink-0 items-center gap-2 px-4 pb-[calc(var(--safe-bottom)+var(--vp-overshoot)+12px)] pt-3">
          <button
            type="button"
            onClick={() => onFilterChange({})}
            className="h-10 shrink-0 rounded-full px-4 text-ui text-white/60 hover:bg-white/10"
          >
            清空
          </button>
          {/* 不是「确定」：条件早就生效了，这颗只是把抽屉收起来看结果 */}
          <button
            type="button"
            onClick={onClose}
            className="btn-glass h-10 flex-1 text-ui font-medium"
          >
            {facets ? `查看 ${facets.total} 部` : "查看结果"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

/** 筛空之后的出路（铁律 2：永不空货架）。 */
export function FilterEmptyState({
  libraryId,
  filter,
  onFilterChange,
}: {
  libraryId: string;
  filter: LibraryFilter;
  onFilterChange: (next: LibraryFilter) => void;
}) {
  const { canManageLibraries } = usePermissions();
  const [relax, setRelax] = useState<LibraryRelax | null>(null);

  useEffect(() => {
    if (!canManageLibraries) {
      return;
    }
    let alive = true;
    getLibraryRelax(libraryId, filter)
      .then((data) => alive && setRelax(data))
      .catch(() => alive && setRelax(null));
    return () => {
      alive = false;
    };
  }, [canManageLibraries, libraryId, filter]);

  const count = filterCount(filter);
  const suggestions = relax?.suggestions ?? [];
  return (
    <div className="mx-6 mt-8 max-w-[34rem] rounded-2xl border border-dashed border-white/[0.14] p-6 max-md:mx-4 max-md:mt-6 max-md:p-4">
      <h3 className="text-body-lg font-semibold text-white">
        没有同时满足这 {count} 个条件的作品
      </h3>
      <p className="mt-1 text-sub text-[var(--text-muted)]">
        {suggestions.length > 0
          ? "放宽一条就能找回内容。"
          : "这几个条件两两之间就没有交集，去掉任意一条也救不回来。"}
      </p>
      <div className="mt-4 flex flex-col gap-1.5">
        {suggestions.map((row) => (
          <button
            key={`${row.dim}:${row.value}`}
            type="button"
            onClick={() => onFilterChange(dropFilterValue(filter, row.dim, row.value))}
            className="glass-row flex items-center gap-3 rounded-xl px-3 py-2 text-left text-sub text-white/80 hover:!bg-[var(--glass-fill-hover)] hover:text-white"
          >
            <span className="min-w-0 flex-1">
              去掉「
              <span className="font-semibold text-white">
                {row.dim_label} = {row.label}
              </span>
              」
            </span>
            <span className="shrink-0 font-mono text-caption tabular-nums text-[var(--info)]">
              → {row.count} 部
            </span>
          </button>
        ))}
      </div>
      <button
        type="button"
        onClick={() => onFilterChange({})}
        className="mt-3 rounded-full px-3 py-1.5 text-caption text-white/50 hover:bg-white/10 hover:text-white"
      >
        清空全部条件
      </button>
    </div>
  );
}
