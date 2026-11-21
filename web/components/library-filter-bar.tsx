"use client";

import { useCallback, useEffect, useState } from "react";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { MultiFilterMenu } from "@/components/filter-menu";
import { ArrowDownIcon, CheckIcon, ChevronDownIcon, FilterIcon } from "@/components/icons";
import {
  ConditionRow,
  FilterEmptyState,
  FilterSheet,
  MoreFiltersPanel,
} from "@/components/library-detail/library-filter-panels";
import {
  filterCount,
  getLibraryFacets,
  isFilterEmpty,
  type LibraryFacets,
  type LibraryFilter,
  type LibraryItemSort,
  type WatchFilter,
} from "@/lib/api/libraries";
import { hasSecondaryFilter } from "@/lib/library-filter-helpers";
import { useIsMobile } from "@/lib/use-media-query";

export { FilterEmptyState };

/**
 * 单库墙的筛选条。
 *
 * 静止态只留信息，不留控件——四个都写着「全部」的下拉不承载任何信息，
 * 它们只是把「什么都没选」这件事说了四遍。所以同一块地方有三种状态，
 * 永远只占一行的量：
 *
 *   静止态：排序（显值、无边框）+ 筛选（唯一一个带框的按钮）
 *   点开后：宽屏多出一行四个维度下拉，用完可收回；窄屏直接拉起底部抽屉，
 *           一二级维度全部平铺成胶囊（见 FilterSheet）
 *   有条件：条件本身顶替控件出现，且与面板开合无关——收起面板不影响
 *           条件可见，否则用户会以为收起就等于取消了筛选
 */
export function LibraryFilterBar({
  libraryId,
  filter,
  onFilterChange,
  sortControl,
  className,
}: {
  libraryId: string;
  filter: LibraryFilter;
  onFilterChange: (next: LibraryFilter) => void;
  /** 排序控件本身由调用方渲染（它是墙的偏好，不属于筛选） */
  sortControl?: React.ReactNode;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [more, setMore] = useState(false);
  const [facets, setFacets] = useState<LibraryFacets | null>(null);
  const mobile = useIsMobile();
  const empty = isFilterEmpty(filter);
  const selectedCount = filterCount(filter);

  const needsAllFacets = more || (mobile && open) || hasSecondaryFilter(filter);
  const secondaryReady = Boolean(
    facets && (facets.ratings.length > 0 || facets.runtimes.length > 0),
  );

  useEffect(() => {
    if (!open && empty) return;
    let alive = true;
    getLibraryFacets(libraryId, filter, needsAllFacets ? "all" : "primary")
      .then((data) => {
        if (alive) setFacets(data);
      })
      .catch(() => {
        // 计数拿不到就不显示数字（下拉仍可用）：数字缺席比数字错了好
        if (alive) setFacets(null);
      });
    return () => {
      alive = false;
    };
  }, [libraryId, filter, open, empty, needsAllFacets]);

  const toggle = useCallback(
    (dim: "genres" | "countries" | "decades", value: string) => {
      const current = (filter[dim] ?? []) as (string | number)[];
      const exists = current.some((v) => String(v) === String(value));
      const next = exists
        ? current.filter((v) => String(v) !== String(value))
        : [...current, value];
      onFilterChange({ ...filter, [dim]: next });
    },
    [filter, onFilterChange],
  );

  const toggleWatch = useCallback(
    (value: string) => {
      // 观看状态是单选：再点一次同一个值就是取消
      onFilterChange({
        ...filter,
        watch: filter.watch === value ? null : (value as WatchFilter),
      });
    },
    [filter, onFilterChange],
  );

  const clearAll = useCallback(() => onFilterChange({}), [onFilterChange]);

  return (
    <div className={className}>
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
          className={`glass-row flex h-8 !w-auto shrink-0 items-center gap-1.5 rounded-full !px-3 text-caption font-medium ${
            open || !empty
              ? "!border-white/[0.16] !bg-[var(--glass-fill-active)] !text-[var(--text)]"
              : "!border-white/[0.12] !bg-[var(--glass-fill)] text-white/90 hover:!bg-[var(--glass-fill-hover)]"
          }`}
        >
          <FilterIcon className="size-3.5 opacity-70" />
          筛选
          {selectedCount > 0 && (
            <span className="font-mono text-caption font-semibold tabular-nums text-white/85">
              {selectedCount}
            </span>
          )}
        </button>
        {sortControl}
      </div>

      {open && !mobile && (
        <div className="mt-2.5 flex items-center gap-2">
          <div className="flex flex-1 flex-wrap items-center gap-2">
            <MultiFilterMenu
              label="类型"
              hint="可多选 · 维度内是「或」"
              selected={(filter.genres ?? []).map(String)}
              options={facets?.genres ?? []}
              onToggle={(v) => toggle("genres", v)}
            />
            <MultiFilterMenu
              label="年代"
              hint="按时间排下来，底条就是分布"
              selected={filter.decades ?? []}
              options={facets?.decades ?? []}
              onToggle={(v) => toggle("decades", v)}
            />
            <MultiFilterMenu
              label="地区"
              hint="可多选 · 维度内是「或」"
              selected={filter.countries ?? []}
              options={facets?.countries ?? []}
              onToggle={(v) => toggle("countries", v)}
            />
            <MultiFilterMenu
              label="观看"
              hint="单选"
              selected={filter.watch ? [filter.watch] : []}
              options={facets?.watch ?? []}
              onToggle={toggleWatch}
            />
          </div>
          <button
            type="button"
            aria-expanded={more}
            onClick={() => setMore((v) => !v)}
            className={`glass-row flex h-8 !w-auto shrink-0 items-center gap-1.5 rounded-full !px-3 text-caption ${
              more ? "!bg-[var(--glass-fill-active)] !text-[var(--text)]" : "text-white/70"
            }`}
          >
            更多筛选
          </button>
        </div>
      )}

      {open && more && !mobile && (
        <MoreFiltersPanel
          facets={facets}
          loading={!secondaryReady}
          filter={filter}
          onFilterChange={onFilterChange}
        />
      )}
      {open && mobile && (
        <FilterSheet
          facets={facets}
          loading={!secondaryReady}
          filter={filter}
          onFilterChange={onFilterChange}
          onToggle={toggle}
          onToggleWatch={toggleWatch}
          onClose={() => setOpen(false)}
        />
      )}

      {/* —— 条件行：与面板开合无关，只要有条件就在 —— */}
      {!empty && (
        <ConditionRow
          filter={filter}
          facets={facets}
          onFilterChange={onFilterChange}
          onClear={clearAll}
        />
      )}
    </div>
  );
}

/** 排序档位 → 展示值。默认档在不同墙上叫法不同，由调用方传入。 */
export const SORT_LABELS: Record<LibraryItemSort, string> = {
  title: "按标题",
  added_at: "最近添加",
  release_date: "按时间",
  probing: "待补探优先",
  rating: "按评分",
  runtime: "按片长",
  size: "按体积",
  last_played: "最近观看",
  random: "随便看看",
};

/**
 * 墙的排序控件——显值、无边框的文字下拉。
 */
export function WallSortControl<T extends string>({
  value,
  options,
  onChange,
  disabled,
  direction,
}: {
  value: T;
  options: readonly (readonly [T, string])[];
  onChange: (next: T) => void;
  /** 扫描补探那几分钟排序被临时接管：如实置灰，不给按了没反应的控件 */
  disabled?: boolean;
  /**
   * 正倒序（不给就不显示方向、也不能切）。按钮上一枚箭头说方向，菜单里当前档
   * 后面用人话写（「短→长」）。再点一次当前档就反转，菜单不关，看得见方向在变。
   */
  direction?: {
    ascending: boolean;
    label: string;
    onToggle: () => void;
  };
}) {
  const current = options.find(([key]) => key === value)?.[1] ?? options[0]?.[1] ?? value;
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          disabled={disabled}
          aria-label="排序"
          title={direction ? `${current} · ${direction.label}` : undefined}
          className="flex h-8 shrink-0 items-center gap-1 rounded-full px-2.5 text-caption text-white/55 hover:bg-white/[0.08] hover:text-white disabled:pointer-events-none disabled:opacity-40 data-[state=open]:bg-white/[0.12] data-[state=open]:text-white"
        >
          <span className="font-semibold text-white/85">{current}</span>
          {direction && (
            <ArrowDownIcon
              className={`size-3 text-white/60 transition-transform ${direction.ascending ? "rotate-180" : ""}`}
            />
          )}
          <ChevronDownIcon className="size-3 text-white/40" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="start"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[12rem] p-1"
        >
          <DropdownMenu.RadioGroup
            value={value}
            onValueChange={(next) => {
              if (next !== value) onChange(next as T);
            }}
          >
            {options.map(([key, label]) => {
              const selected = key === value;
              return (
                <DropdownMenu.RadioItem
                  key={key}
                  value={key}
                  onSelect={(event) => {
                    if (!selected || !direction) return;
                    event.preventDefault();
                    direction.onToggle();
                  }}
                  aria-label={
                    selected && direction ? `${label}，${direction.label}，再点一次反转顺序` : undefined
                  }
                  className="glass-row nav-item flex cursor-pointer items-center justify-between gap-3 px-3 py-2 text-sub outline-none data-[highlighted]:!bg-[var(--glass-fill-hover)]"
                >
                  {label}
                  <span className="flex items-center gap-1.5 text-caption text-[var(--info)]">
                    {selected && direction && (
                      <>
                        {direction.label}
                        <ArrowDownIcon
                          className={`size-3 ${direction.ascending ? "rotate-180" : ""}`}
                        />
                      </>
                    )}
                    <DropdownMenu.ItemIndicator>
                      <CheckIcon className="size-3.5 text-[var(--info)]" />
                    </DropdownMenu.ItemIndicator>
                  </span>
                </DropdownMenu.RadioItem>
              );
            })}
          </DropdownMenu.RadioGroup>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
