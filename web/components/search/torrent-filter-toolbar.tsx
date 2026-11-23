"use client";

import { useEffect, useRef, useState } from "react";

import { LayersIcon, ListIcon, PhotoIcon } from "@/components/icons";
import type { SiteSearchStatus } from "@/lib/api/search";
import {
  emptyFilters,
  type Facets,
  type Filters,
  type ResultView,
  SHEET_KEYS,
  type SheetKey,
  sheetSelectionCount,
  SMART_SORT_OPTIONS,
  SORT_OPTIONS,
  type SortKey,
  type SortState,
  toggleIn,
} from "@/lib/torrent-search-view";

const CHIP_ACTIVE_CLS =
  "border-white/[0.2] bg-white/[0.14] text-[var(--text)] shadow-[inset_0_1px_0_rgba(255,255,255,0.1)]";
const CHIP_IDLE_CLS =
  "border-white/[0.08] bg-white/[0.035] text-[var(--text-muted)] hover:border-white/[0.15] hover:bg-white/[0.07] hover:text-[var(--text)]";
const SEGMENT_ACTIVE_CLS =
  "bg-white/[0.14] font-medium text-[var(--text)] shadow-[inset_0_1px_0_rgba(255,255,255,0.1)]";
const SEGMENT_IDLE_CLS =
  "text-[var(--text-muted)] hover:bg-white/[0.05] hover:text-[var(--text)]";

function FacetChip({
  label,
  count,
  active,
  onToggle,
}: {
  label: string;
  count: number;
  active: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={active}
      className={`h-7 rounded-full border px-2.5 text-caption transition-all ${active ? CHIP_ACTIVE_CLS : CHIP_IDLE_CLS}`}
    >
      {label}
      <span className="tnum ml-1 opacity-60">{count}</span>
    </button>
  );
}

/** 浮层退出交互：Esc 或点击 ref 区域（浮层+触发器）以外即关闭。 */
function useDismiss(
  ref: React.RefObject<HTMLElement | null>,
  onClose: () => void,
  active = true,
) {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCloseRef.current();
    };
    const onDown = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onCloseRef.current();
    };
    window.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onDown);
    };
  }, [ref, active]);
}

function FacetDropdown<T extends string | number>({
  label,
  options,
  active,
  onToggle,
}: {
  label: string;
  options: { value: T; label: string; count: number }[];
  active: Set<T>;
  onToggle: (value: T) => void;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useDismiss(rootRef, () => setOpen(false), open);

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className={`flex h-7 items-center gap-1.5 rounded-full border px-3 text-caption transition-all ${
          active.size > 0 ? CHIP_ACTIVE_CLS : CHIP_IDLE_CLS
        }`}
      >
        {label}
        {active.size > 0 && (
          <span className="tnum rounded-full bg-[var(--accent)]/25 px-1.5 text-micro">
            {active.size}
          </span>
        )}
        <span className={`text-micro opacity-70 transition-transform ${open ? "rotate-180" : ""}`}>
          ▾
        </span>
      </button>
      {open && (
        <div className="popover-surface absolute left-0 top-full z-30 mt-2 max-h-[46dvh] w-max max-w-[420px] overflow-y-auto rounded-2xl p-3 max-md:max-w-[calc(100vw-2rem)]">
          <div className="flex flex-wrap gap-1.5">
            {options.map((o) => (
              <FacetChip
                key={o.value}
                label={o.label}
                count={o.count}
                active={active.has(o.value)}
                onToggle={() => onToggle(o.value)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function SortDropdown({
  sort,
  smartOptions,
  onChange,
}: {
  sort: SortState;
  smartOptions: { key: SortKey; label: string }[];
  onChange: (s: SortState) => void;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useDismiss(rootRef, () => setOpen(false), open);
  const arrow = sort.dir === "desc" ? "↓" : "↑";
  const label =
    [...SORT_OPTIONS, ...SMART_SORT_OPTIONS].find((o) => o.key === sort.key)?.label ?? sort.key;
  const pick = (key: SortKey) => {
    onChange(
      key === sort.key
        ? { key, dir: sort.dir === "desc" ? "asc" : "desc" }
        : { key, dir: "desc" },
    );
    setOpen(false);
  };
  const item = (o: { key: SortKey; label: string }) => (
    <button
      key={o.key}
      type="button"
      onClick={() => pick(o.key)}
      className={`flex w-full items-center justify-between rounded-lg px-2.5 py-1.5 text-left text-caption transition-colors ${
        o.key === sort.key
          ? "bg-white/[0.09] text-[var(--text)]"
          : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-[var(--text)]"
      }`}
    >
      {o.label}
      {o.key === sort.key && <span className="text-sub">{arrow}</span>}
    </button>
  );

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-label={`排序：${label}${sort.dir === "desc" ? "降序" : "升序"}`}
        className={`flex h-7 items-center gap-1.5 rounded-full border px-3 text-caption transition-all ${CHIP_IDLE_CLS}`}
      >
        {label}
        <span className="text-sub">{arrow}</span>
        <span className={`text-micro opacity-70 transition-transform ${open ? "rotate-180" : ""}`}>
          ▾
        </span>
      </button>
      {open && (
        <div className="popover-surface absolute left-0 top-full z-30 mt-2 w-44 rounded-2xl p-1.5">
          <p className="px-2.5 pb-1 pt-1.5 text-micro text-[var(--text-faint)]">常规</p>
          {SORT_OPTIONS.map(item)}
          {smartOptions.length > 0 && (
            <>
              <p className="px-2.5 pb-1 pt-2 text-micro text-[var(--text-faint)]">智能</p>
              {smartOptions.map(item)}
            </>
          )}
          <p className="border-t border-white/[0.08] px-2.5 pb-1 pt-2 text-micro text-[var(--text-faint)]">
            点击当前项可切换升降序
          </p>
        </div>
      )}
    </div>
  );
}

function SheetGroup({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <p className="mb-1.5 text-caption font-medium text-[var(--text-faint)]">{title}</p>
      <div className="flex flex-wrap gap-1.5">{children}</div>
    </div>
  );
}

function FilterSheet({
  sites,
  facets,
  filters,
  onChange,
  resultCount,
  onClose,
}: {
  sites: SiteSearchStatus[];
  facets: Facets;
  filters: Filters;
  onChange: (f: Filters) => void;
  resultCount: number;
  onClose: () => void;
}) {
  const toggle = (key: SheetKey, value: string | number) =>
    onChange({ ...filters, [key]: toggleIn(filters[key] as Set<string | number>, value) });

  const okSites = sites.filter((s) => !s.error);

  return (
    <div className="popover-surface absolute right-0 top-full z-30 mt-2 max-h-[60dvh] w-[480px] max-w-[82vw] overflow-y-auto rounded-2xl p-4">
      <div className="mb-4 flex items-center justify-between">
        <div>
          <p className="text-ui font-medium text-[var(--text)]">筛选结果</p>
          <p className="mt-0.5 text-caption text-[var(--text-faint)]">可多选，分类之间组合生效</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          className={`grid size-7 place-items-center rounded-full border text-body ${CHIP_IDLE_CLS}`}
          aria-label="关闭筛选"
        >
          ×
        </button>
      </div>
      <div className="space-y-3.5">
        {okSites.length > 0 && (
          <SheetGroup title="站点">
            {okSites.map((s) => (
              <FacetChip
                key={s.site_id}
                label={s.site_name}
                count={facets.sites.get(s.site_id) ?? 0}
                active={filters.site.has(s.site_id)}
                onToggle={() => toggle("site", s.site_id)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.years.length > 0 && (
          <SheetGroup title="年份">
            {facets.years.map((y) => (
              <FacetChip
                key={y.value}
                label={String(y.value)}
                count={y.count}
                active={filters.year.has(y.value)}
                onToggle={() => toggle("year", y.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.seasons.length > 0 && (
          <SheetGroup title="季">
            {facets.seasons.map((s) => (
              <FacetChip
                key={s.value}
                label={`第${s.value}季`}
                count={s.count}
                active={filters.season.has(s.value)}
                onToggle={() => toggle("season", s.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.episodes.length > 0 && (
          <SheetGroup title="集">
            {facets.episodes.map((ep) => (
              <FacetChip
                key={ep.value}
                label={`第${ep.value}集`}
                count={ep.count}
                active={filters.episode.has(ep.value)}
                onToggle={() => toggle("episode", ep.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.source.length > 0 && (
          <SheetGroup title="片源">
            {facets.source.map((v) => (
              <FacetChip
                key={v.value}
                label={v.value}
                count={v.count}
                active={filters.source.has(v.value)}
                onToggle={() => toggle("source", v.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.codec.length > 0 && (
          <SheetGroup title="视频编码">
            {facets.codec.map((v) => (
              <FacetChip
                key={v.value}
                label={v.value}
                count={v.count}
                active={filters.codec.has(v.value)}
                onToggle={() => toggle("codec", v.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.hdr.length > 0 && (
          <SheetGroup title="HDR">
            {facets.hdr.map((v) => (
              <FacetChip
                key={v.value}
                label={v.value}
                count={v.count}
                active={filters.hdr.has(v.value)}
                onToggle={() => toggle("hdr", v.value)}
              />
            ))}
          </SheetGroup>
        )}
        {facets.groups.length > 0 && (
          <SheetGroup title="压制组">
            {facets.groups.map((v) => (
              <FacetChip
                key={v.value}
                label={v.value}
                count={v.count}
                active={filters.group.has(v.value)}
                onToggle={() => toggle("group", v.value)}
              />
            ))}
          </SheetGroup>
        )}
      </div>

      <div className="mt-4 flex items-center justify-between border-t border-white/[0.08] pt-3">
        <button
          type="button"
          onClick={() => onChange(emptyFilters())}
          className="btn-glass h-8 px-3 text-caption text-[var(--text-muted)]"
        >
          清除全部
        </button>
        <button
          type="button"
          onClick={onClose}
          className="btn-accent h-8 rounded-full px-4 text-caption font-medium"
        >
          查看 {resultCount} 条结果
        </button>
      </div>
    </div>
  );
}

/** 第一层：常驻工具栏（排序 + 分辨率 + 高频维度下拉 + 视图切换 + 筛选弹层）。 */
export function FilterToolbar({
  sites,
  facets,
  filters,
  onChange,
  sort,
  smartSortKeys,
  onSortChange,
  resultCount,
  view,
  onViewChange,
}: {
  sites: SiteSearchStatus[];
  facets: Facets;
  filters: Filters;
  onChange: (f: Filters) => void;
  sort: SortState;
  smartSortKeys: SortKey[];
  onSortChange: (s: SortState) => void;
  resultCount: number;
  view: ResultView;
  onViewChange: (v: ResultView) => void;
}) {
  const [sheetOpen, setSheetOpen] = useState(false);
  const sheetRef = useRef<HTMLDivElement>(null);
  useDismiss(sheetRef, () => setSheetOpen(false), sheetOpen);
  const badge = sheetSelectionCount(filters);
  const smartOptions = SMART_SORT_OPTIONS.filter(
    (o) => smartSortKeys.includes(o.key) || o.key === sort.key,
  );

  return (
    <div className="mt-3 flex flex-wrap items-center gap-3 rounded-2xl border border-white/[0.07] bg-black/[0.14] p-2.5 shadow-[inset_0_1px_0_rgba(255,255,255,0.04)] backdrop-blur-xl">
      <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
        <SortDropdown sort={sort} smartOptions={smartOptions} onChange={onSortChange} />

        {facets.resolution.slice(0, 3).map(({ value, count }, i) => (
          <span key={value} className={i >= 2 ? "max-md:hidden" : undefined}>
            <FacetChip
              label={value}
              count={count}
              active={filters.resolution.has(value)}
              onToggle={() =>
                onChange({ ...filters, resolution: toggleIn(filters.resolution, value) })
              }
            />
          </span>
        ))}

        <div className="flex items-center gap-2 max-md:hidden">
          {facets.years.length >= 2 && (
            <FacetDropdown
              label="年份"
              options={facets.years.map((y) => ({
                value: y.value,
                label: String(y.value),
                count: y.count,
              }))}
              active={filters.year}
              onToggle={(v) => onChange({ ...filters, year: toggleIn(filters.year, v) })}
            />
          )}
          {facets.seasons.length >= 2 && (
            <FacetDropdown
              label="季"
              options={facets.seasons.map((s) => ({
                value: s.value,
                label: `第${s.value}季`,
                count: s.count,
              }))}
              active={filters.season}
              onToggle={(v) => onChange({ ...filters, season: toggleIn(filters.season, v) })}
            />
          )}
          {facets.groups.length >= 2 && (
            <FacetDropdown
              label="压制组"
              options={facets.groups.map((g) => ({
                value: g.value,
                label: g.value,
                count: g.count,
              }))}
              active={filters.group}
              onToggle={(v) => onChange({ ...filters, group: toggleIn(filters.group, v) })}
            />
          )}
        </div>
      </div>

      <div className="relative ml-auto flex flex-wrap items-center justify-end gap-1.5">
        <div className="flex items-center rounded-full border border-white/[0.08] bg-black/[0.16] p-0.5">
          {(
            [
              { v: "group", label: "分组", Icon: LayersIcon, hint: "分组：按作品聚合，组内是各版本" },
              { v: "list", label: "列表", Icon: ListIcon, hint: "列表：平铺展示每条种子" },
              { v: "poster", label: "图览", Icon: PhotoIcon, hint: "图览：带海报的结果以图墙展示" },
            ] as const
          ).map(({ v, label, Icon, hint }) => (
            <button
              key={v}
              type="button"
              aria-pressed={view === v}
              aria-label={label}
              title={hint}
              onClick={() => onViewChange(v)}
              className={`grid h-6 w-8 place-items-center rounded-full transition-all ${view === v ? SEGMENT_ACTIVE_CLS : SEGMENT_IDLE_CLS}`}
            >
              <Icon className="size-3.5" />
            </button>
          ))}
        </div>

        <div ref={sheetRef} className="relative">
          <button
            type="button"
            onClick={() => setSheetOpen((v) => !v)}
            aria-expanded={sheetOpen}
            className={`flex h-7 items-center gap-1.5 rounded-full border px-3 text-caption transition-all ${
              badge > 0 ? CHIP_ACTIVE_CLS : CHIP_IDLE_CLS
            }`}
          >
            筛选
            {badge > 0 && (
              <span className="tnum rounded-full bg-[var(--accent)]/25 px-1.5 text-micro">
                {badge}
              </span>
            )}
            <span className={`text-micro opacity-70 transition-transform ${sheetOpen ? "rotate-180" : ""}`}>
              ▾
            </span>
          </button>

          {sheetOpen && (
            <FilterSheet
              sites={sites}
              facets={facets}
              filters={filters}
              onChange={onChange}
              resultCount={resultCount}
              onClose={() => setSheetOpen(false)}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/** 第三层：已应用条件回显行。 */
export function AppliedChips({
  sites,
  filters,
  onChange,
}: {
  sites: SiteSearchStatus[];
  filters: Filters;
  onChange: (f: Filters) => void;
}) {
  if (sheetSelectionCount(filters) === 0) return null;

  const siteName = (id: string) =>
    sites.find((s) => s.site_id === id)?.site_name ?? id;
  const labelOf = (key: SheetKey, value: string | number): string => {
    if (key === "site") return siteName(String(value));
    if (key === "season") return `第${value}季`;
    if (key === "episode") return `第${value}集`;
    return String(value);
  };

  const chips = SHEET_KEYS.flatMap((key) =>
    [...filters[key]].map((value) => ({ key, value })),
  );

  return (
    <div className="mt-2 flex flex-wrap items-center gap-1.5 px-1">
      <span className="text-caption text-[var(--text-faint)]">生效条件</span>
      {chips.map(({ key, value }) => (
        <button
          key={`${key}:${value}`}
          type="button"
          onClick={() =>
            onChange({ ...filters, [key]: toggleIn(filters[key] as Set<string | number>, value) })
          }
          className={`group flex h-6 items-center gap-1 rounded-full border px-2 text-caption transition-all hover:bg-white/[0.2] ${CHIP_ACTIVE_CLS}`}
        >
          {labelOf(key, value)}
          <span className="opacity-60 transition-opacity group-hover:opacity-100">×</span>
        </button>
      ))}
      <button
        type="button"
        onClick={() => onChange(emptyFilters())}
        className="ml-1 text-caption text-[var(--text-faint)] transition-colors hover:text-[var(--text)]"
      >
        清除全部
      </button>
    </div>
  );
}
