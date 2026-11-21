"use client";

import { useMemo, useState } from "react";

import { TorrentRow } from "@/components/search/torrent-row";
import { Tooltip } from "@/components/tooltip";
import type { TorrentHit } from "@/lib/api/search";
import {
  type EntityGroup,
  groupHitsByEntity,
  maxResolution,
  MEDIA_TYPE_LABEL,
  toggleIn,
  torrentIdOf,
} from "@/lib/torrent-search-view";

/** 组内默认展示的版本数上限；超出折叠成「展开其余 N 个版本」。 */
const GROUP_ROW_CAP = 4;

/**
 * 按作品分组的结果视图（识别出 ≥2 部作品时替代平铺）。
 */
export function GroupedResults({
  hits,
  entities,
}: {
  hits: TorrentHit[];
  entities: Map<string, EntityGroup>;
}) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [uncapped, setUncapped] = useState<Set<string>>(() => new Set());

  const buckets = useMemo(() => groupHitsByEntity(hits), [hits]);

  return (
    <div className="space-y-4">
      {buckets.map(({ key, rows }) => {
        const meta = entities.get(key) ?? null;
        const open = !collapsed.has(key);
        const showAll = uncapped.has(key) || rows.length <= GROUP_ROW_CAP;
        const shown = showAll ? rows : rows.slice(0, GROUP_ROW_CAP);
        return (
          <section key={key}>
            <GroupHeader
              meta={meta}
              rows={rows}
              open={open}
              onToggle={() => setCollapsed((prev) => toggleIn(prev, key))}
            />
            {open && (
              <>
                <ul className="mt-2 space-y-2 pl-4">
                  {shown.map((hit) => (
                    <TorrentRow
                      key={`${hit.site_id}:${torrentIdOf(hit)}`}
                      hit={hit}
                      grouped
                    />
                  ))}
                </ul>
                {!showAll && (
                  <button
                    type="button"
                    onClick={() => setUncapped((prev) => toggleIn(prev, key))}
                    className="ml-4 mt-2 px-1 py-0.5 text-sub text-[var(--accent-2)] transition-colors hover:text-[var(--text)]"
                  >
                    展开其余 {rows.length - GROUP_ROW_CAP} 个版本 ▾
                  </button>
                )}
              </>
            )}
          </section>
        );
      })}
    </div>
  );
}

/** 组头：片名 + 年份/类型 + 组内聚合徽标（版本数、最高分辨率、免费数）。 */
function GroupHeader({
  meta,
  rows,
  open,
  onToggle,
}: {
  meta: EntityGroup | null;
  rows: TorrentHit[];
  open: boolean;
  onToggle: () => void;
}) {
  const unparsed = meta === null;
  const freeCount = rows.filter((h) => h.free).length;
  const topRes = maxResolution(rows);
  const info = unparsed
    ? "按原始名展示"
    : [meta.year, MEDIA_TYPE_LABEL[meta.mediaType]].filter(Boolean).join(" · ");

  const header = (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      className="flex w-full items-center gap-3 rounded-2xl border border-white/[0.09] bg-white/[0.045] px-4 py-2.5 text-left backdrop-blur-xl transition-colors hover:border-white/[0.16] hover:bg-white/[0.07]"
    >
      <span
        className={`shrink-0 text-micro text-[var(--text-faint)] transition-transform ${open ? "rotate-90" : ""}`}
      >
        ▶
      </span>
      <span className="min-w-0 flex-1 truncate">
        <span className="text-body font-semibold text-[var(--text)]">
          {unparsed ? "未识别" : meta.title}
        </span>
        {info && (
          <span className="tnum ml-2 text-sub text-[var(--text-faint)]">{info}</span>
        )}
      </span>
      <span className="flex shrink-0 items-center gap-1.5">
        <span className="tnum rounded-md bg-white/[0.05] px-1.5 py-0.5 text-micro text-[var(--text-muted)]">
          {rows.length} 个版本
        </span>
        {topRes && (
          <span className="rounded-md bg-white/[0.05] px-1.5 py-0.5 text-micro text-[var(--accent-2)]">
            最高 {topRes}
          </span>
        )}
        {freeCount > 0 && (
          <span className="tnum rounded-md bg-[var(--ok)]/15 px-1.5 py-0.5 text-micro font-semibold text-[#79d193]">
            {freeCount} 个免费
          </span>
        )}
      </span>
    </button>
  );

  const sample = rows[0];
  if (unparsed || !sample?.title) return header;
  return (
    <Tooltip placement="top-start" content={<p>{sample.title}</p>}>
      {header}
    </Tooltip>
  );
}
