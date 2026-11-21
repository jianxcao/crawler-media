import type { Release, TorrentHit } from "@/lib/api/search";
import { formatBytes } from "./format.ts";

/* —— TorrentHit / Release 基础访问器 —— */

/** 种子唯一标识：优先站内 ID，缺省回退种子下载链接。 */
export function torrentIdOf(hit: TorrentHit): string {
  return hit.id ?? hit.enclosure;
}

/** 根据季/集信息判断资源类型；有季或集即按剧集处理。 */
export function mediaTypeOf(release: Release): "movie" | "tv" {
  return release.season != null || release.episode != null ? "tv" : "movie";
}

/** 展开单集或连集范围（episode..episode_to）为集号数组。 */
export function episodesOf(release: Release): number[] {
  if (release.episode == null) return [];
  if (release.episode_to != null && release.episode_to > release.episode) {
    return Array.from(
      { length: release.episode_to - release.episode + 1 },
      (_, i) => release.episode! + i,
    );
  }
  return [release.episode];
}

/** 标题解析出的片名；解析失败（空串）返回 null。 */
export function parsedTitleOf(hit: TorrentHit): string | null {
  return hit.release.title || null;
}

/** 字节数格式化为体积展示串；空值或 0 返回 null。 */
export function torrentSizeLabel(sizeBytes: number | null): string | null {
  return sizeBytes != null && sizeBytes > 0 ? formatBytes(sizeBytes) : null;
}

/** 耗时展示：统一到秒（一位小数），站点状态弹层用。 */
export function formatElapsed(ms: number): string {
  return `${(ms / 1000).toFixed(1)} 秒`;
}

/** 影视类型的展示文案。 */
export const MEDIA_TYPE_LABEL: Record<"movie" | "tv", string> = {
  movie: "电影",
  tv: "剧集",
};

/* —— 排序与视图状态 —— */

export type SortKey =
  | "seeders"
  | "time"
  | "size"
  | "snatched"
  | "complete"
  | "quality"
  | "free";

export const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: "seeders", label: "做种数" },
  { key: "time", label: "发布时间" },
  { key: "size", label: "体积" },
  { key: "snatched", label: "完成数" },
];

export const SMART_SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: "complete", label: "全集优先" },
  { key: "quality", label: "画质优先" },
  { key: "free", label: "免费优先" },
];

export interface SortState {
  key: SortKey;
  dir: "desc" | "asc";
}

/** 从地址栏的 sort=key:dir 参数还原排序态，非法/缺失时回落到默认（做种数降序）。 */
export function readSortFromUrl(search?: string): SortState {
  const fallback: SortState = { key: "seeders", dir: "desc" };
  const rawSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  if (!rawSearch) return fallback;
  const [key, dir] = (new URLSearchParams(rawSearch).get("sort") ?? "").split(":");
  if (
    [...SORT_OPTIONS, ...SMART_SORT_OPTIONS].some((o) => o.key === key) &&
    (dir === "asc" || dir === "desc")
  ) {
    return { key: key as SortKey, dir };
  }
  return fallback;
}

/** 结果展示视图：group=按作品分组，list=平铺种子列表（默认），poster=图览。 */
export type ResultView = "group" | "list" | "poster";

/** 视图初值：地址栏 view=group / poster=1 优先，否则默认 list。 */
export function initialView(search?: string): ResultView {
  const rawSearch = search ?? (typeof window !== "undefined" ? window.location.search : "");
  if (!rawSearch) return "list";
  const params = new URLSearchParams(rawSearch);
  if (params.get("view") === "group") return "group";
  if (params.get("poster") === "1") return "poster";
  return "list";
}

/**
 * 剧集完整度层级（越大越完整）：
 * 3=整季包（有季无集），2=多集区间，1=单集，0=电影或无季集信息。
 */
export function completenessTier(release: Release): number {
  if (release.season != null && release.episode == null) return 3;
  const eps = episodesOf(release);
  if (eps.length > 1) return 2;
  if (eps.length === 1) return 1;
  return 0;
}

/** 片源档位（画质排序第二级）：碟源 > WEB > 电视录制 > DVD。 */
export const SOURCE_RANK: Record<string, number> = {
  "UHD Blu-ray": 7,
  "Blu-ray": 6,
  "WEB-DL": 5,
  BDRip: 4,
  WEBRip: 3,
  "HD-DVD": 3,
  HDTV: 2,
  HDTVRip: 2,
  HDRip: 2,
  TVRip: 1,
  DVDRip: 1,
  DVD: 1,
};

/** 排序向量：逐级字典序比较（前级相等才看后级）。 */
export function sortVector(hit: TorrentHit, key: SortKey): number[] {
  const r = hit.release;
  const seeders = hit.seeders ?? 0;
  switch (key) {
    case "time":
      return [hit.upload_time ? Date.parse(hit.upload_time) : -Infinity];
    case "size":
      return [hit.size_bytes ?? 0];
    case "snatched":
      return [hit.snatched ?? 0];
    case "complete":
      return [completenessTier(r), r.season != null ? 1 : 0, seeders];
    case "quality":
      return [
        r.resolution ? parseInt(r.resolution, 10) || 0 : 0,
        r.source ? (SOURCE_RANK[r.source] ?? 0) : 0,
        seeders,
      ];
    case "free":
      return [hit.free ? 2 : 0, hit.hr === false ? 1 : 0, seeders];
    default:
      return [seeders];
  }
}

/** 预计算排序向量后排序，避免比较器内重复分配数组。 */
export function sortHits(items: TorrentHit[], sort: SortState): TorrentHit[] {
  const dir = sort.dir === "asc" ? 1 : -1;
  const decorated = items.map((hit) => ({ hit, vector: sortVector(hit, sort.key) }));
  decorated.sort((x, y) => {
    for (let i = 0; i < x.vector.length; i++) {
      if (x.vector[i] !== y.vector[i]) return (x.vector[i] - y.vector[i]) * dir;
    }
    return 0;
  });
  return decorated.map((d) => d.hit);
}

/** 按结果集类型构成动态生成智能排序键：剧集 >=50% 给 complete，电影 >50% 给 quality，free 常驻。 */
export function computeSmartSortKeys(items: TorrentHit[]): SortKey[] {
  let tv = 0;
  let movie = 0;
  for (const h of items) {
    if (mediaTypeOf(h.release) === "tv") tv++;
    else movie++;
  }
  const keys: SortKey[] = [];
  const typed = tv + movie;
  if (typed > 0 && tv >= typed / 2) keys.push("complete");
  if (typed > 0 && movie > typed / 2) keys.push("quality");
  keys.push("free");
  return keys;
}

/* —— 筛选与自排除分面计数 —— */

export interface Filters {
  resolution: Set<string>;
  site: Set<string>;
  year: Set<number>;
  season: Set<number>;
  episode: Set<number>;
  source: Set<string>;
  codec: Set<string>;
  hdr: Set<string>;
  group: Set<string>;
}

export function emptyFilters(): Filters {
  return {
    resolution: new Set(),
    site: new Set(),
    year: new Set(),
    season: new Set(),
    episode: new Set(),
    source: new Set(),
    codec: new Set(),
    hdr: new Set(),
    group: new Set(),
  };
}

export const SHEET_KEYS = [
  "site",
  "year",
  "season",
  "episode",
  "source",
  "codec",
  "hdr",
  "group",
] as const;

export type SheetKey = (typeof SHEET_KEYS)[number];
export type FilterDim = keyof Filters;

export function toggleIn<T>(set: Set<T>, value: T): Set<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

export function sheetSelectionCount(f: Filters): number {
  return SHEET_KEYS.reduce((sum, key) => sum + f[key].size, 0);
}

export function hasActiveFilters(f: Filters): boolean {
  return f.resolution.size > 0 || sheetSelectionCount(f) > 0;
}

const FILTER_DIMENSIONS: { dim: FilterDim; pass: (hit: TorrentHit, f: Filters) => boolean }[] = [
  {
    dim: "resolution",
    pass: (hit, f) =>
      !f.resolution.size || (!!hit.release.resolution && f.resolution.has(hit.release.resolution)),
  },
  { dim: "site", pass: (hit, f) => !f.site.size || f.site.has(hit.site_id) },
  {
    dim: "year",
    pass: (hit, f) => !f.year.size || (hit.release.year != null && f.year.has(hit.release.year)),
  },
  {
    dim: "season",
    pass: (hit, f) =>
      !f.season.size || (hit.release.season != null && f.season.has(hit.release.season)),
  },
  {
    dim: "episode",
    pass: (hit, f) => !f.episode.size || episodesOf(hit.release).some((e) => f.episode.has(e)),
  },
  {
    dim: "source",
    pass: (hit, f) => !f.source.size || (!!hit.release.source && f.source.has(hit.release.source)),
  },
  {
    dim: "codec",
    pass: (hit, f) => !f.codec.size || (!!hit.release.codec && f.codec.has(hit.release.codec)),
  },
  {
    dim: "hdr",
    pass: (hit, f) => !f.hdr.size || (!!hit.release.hdr && f.hdr.has(hit.release.hdr)),
  },
  {
    dim: "group",
    pass: (hit, f) => !f.group.size || (!!hit.release.group && f.group.has(hit.release.group)),
  },
];

export function matchesFilters(hit: TorrentHit, f: Filters): boolean {
  return FILTER_DIMENSIONS.every((d) => d.pass(hit, f));
}

export interface FacetValue {
  value: string;
  count: number;
}

export interface NumericFacetValue {
  value: number;
  count: number;
}

export interface Facets {
  resolution: FacetValue[];
  years: NumericFacetValue[];
  seasons: NumericFacetValue[];
  episodes: NumericFacetValue[];
  source: FacetValue[];
  codec: FacetValue[];
  hdr: FacetValue[];
  groups: FacetValue[];
  sites: Map<string, number>;
}

interface FacetMaps {
  resolution: Map<string, number>;
  source: Map<string, number>;
  codec: Map<string, number>;
  hdr: Map<string, number>;
  groups: Map<string, number>;
  sites: Map<string, number>;
  years: Map<number, number>;
  seasons: Map<number, number>;
  episodes: Map<number, number>;
}

function collectFacetMaps(items: TorrentHit[], filters: Filters | null): FacetMaps {
  const maps: FacetMaps = {
    resolution: new Map(),
    source: new Map(),
    codec: new Map(),
    hdr: new Map(),
    groups: new Map(),
    sites: new Map(),
    years: new Map(),
    seasons: new Map(),
    episodes: new Map(),
  };
  const bump = (map: Map<string, number>, key: string) =>
    map.set(key, (map.get(key) ?? 0) + 1);
  const bumpNum = (map: Map<number, number>, key: number) =>
    map.set(key, (map.get(key) ?? 0) + 1);

  for (const hit of items) {
    let only: FilterDim | null = null;
    if (filters) {
      let failedCount = 0;
      for (const d of FILTER_DIMENSIONS) {
        if (d.pass(hit, filters)) continue;
        failedCount += 1;
        if (failedCount > 1) break;
        only = d.dim;
      }
      if (failedCount > 1) continue;
      if (failedCount === 0) only = null;
    }
    const want = (dim: FilterDim) => only === null || only === dim;

    if (want("site")) bump(maps.sites, hit.site_id);
    const r = hit.release;
    if (want("year") && r.year !== null) bumpNum(maps.years, r.year);
    if (want("season") && r.season !== null) bumpNum(maps.seasons, r.season);
    if (want("episode")) for (const e of episodesOf(r)) bumpNum(maps.episodes, e);
    if (want("resolution") && r.resolution) bump(maps.resolution, r.resolution);
    if (want("source") && r.source) bump(maps.source, r.source);
    if (want("codec") && r.codec) bump(maps.codec, r.codec);
    if (want("hdr") && r.hdr) bump(maps.hdr, r.hdr);
    if (want("group") && r.group) bump(maps.groups, r.group);
  }
  return maps;
}

export function aggregateFacets(items: TorrentHit[], filters: Filters): Facets {
  const full = collectFacetMaps(items, null);
  const counts = hasActiveFilters(filters) ? collectFacetMaps(items, filters) : full;

  const toSorted = (key: keyof FacetMaps & string, cap: number): FacetValue[] =>
    [...(full[key] as Map<string, number>).entries()]
      .sort((x, y) => y[1] - x[1] || x[0].localeCompare(y[0]))
      .slice(0, cap)
      .map(([value]) => ({
        value,
        count: (counts[key] as Map<string, number>).get(value) ?? 0,
      }));
  const toNumeric = (key: "years" | "seasons" | "episodes", order: 1 | -1): NumericFacetValue[] =>
    [...full[key].entries()]
      .sort((x, y) => (x[0] - y[0]) * order)
      .map(([value]) => ({ value, count: counts[key].get(value) ?? 0 }));

  return {
    resolution: toSorted("resolution", 8),
    years: toNumeric("years", -1),
    seasons: toNumeric("seasons", 1),
    episodes: toNumeric("episodes", 1),
    source: toSorted("source", 50),
    codec: toSorted("codec", 50),
    hdr: toSorted("hdr", 50),
    groups: toSorted("groups", 50),
    sites: counts.sites,
  };
}

/* —— 作品聚合 —— */

export const ENTITY_UNPARSED = "__unparsed__";

export interface EntityGroup {
  key: string;
  title: string;
  year: number | null;
  mediaType: "movie" | "tv";
}

export function entityKeyOf(hit: TorrentHit): string {
  const title = parsedTitleOf(hit);
  if (!title) return ENTITY_UNPARSED;
  const type = mediaTypeOf(hit.release);
  if (type === "tv") return `tv|${title}`;
  return `movie|${title}|${hit.release.year ?? "?"}`;
}

export function collectEntities(items: TorrentHit[]): Map<string, EntityGroup> {
  const groups = new Map<string, EntityGroup>();
  for (const hit of items) {
    const key = entityKeyOf(hit);
    if (key === ENTITY_UNPARSED) continue;
    const r = hit.release;
    const g = groups.get(key);
    if (g) {
      g.year ??= r.year ?? null;
    } else {
      groups.set(key, {
        key,
        title: r.title,
        year: r.year ?? null,
        mediaType: mediaTypeOf(r),
      });
    }
  }
  return groups;
}

export function groupHitsByEntity(hits: TorrentHit[]): { key: string; rows: TorrentHit[] }[] {
  const buckets: { key: string; rows: TorrentHit[] }[] = [];
  const indexOf = new Map<string, number>();
  for (const hit of hits) {
    const key = entityKeyOf(hit);
    const i = indexOf.get(key);
    if (i === undefined) {
      indexOf.set(key, buckets.length);
      buckets.push({ key, rows: [hit] });
    } else {
      buckets[i].rows.push(hit);
    }
  }
  const unparsedAt = indexOf.get(ENTITY_UNPARSED);
  if (unparsedAt !== undefined && unparsedAt !== buckets.length - 1) {
    const [u] = buckets.splice(unparsedAt, 1);
    buckets.push(u);
  }
  return buckets;
}

/* —— 展示辅助 —— */

export function maxResolution(rows: TorrentHit[]): string | null {
  let best: string | null = null;
  let bestNum = -1;
  for (const hit of rows) {
    const res = hit.release.resolution;
    if (!res) continue;
    const num = parseInt(res, 10);
    if (Number.isFinite(num) && num > bestNum) {
      bestNum = num;
      best = res;
    }
  }
  return best;
}

export function seederTone(n: number): string {
  if (n === 0) return "text-[#ff9a9a]";
  if (n < 5) return "text-[#fbbf24]";
  if (n >= 100) return "font-semibold text-[var(--ok)]";
  return "text-[#79d193]";
}

export function formatNumList(nums: number[]): string {
  const sorted = [...nums].sort((a, b) => a - b);
  const contiguous = sorted.every((n, i) => i === 0 || n === sorted[i - 1] + 1);
  if (sorted.length > 1 && contiguous) return `${sorted[0]}-${sorted[sorted.length - 1]}`;
  return sorted.join("、");
}

export function seasonEpLabel(release: Release): string | null {
  const pad = (n: number) => String(n).padStart(2, "0");
  const eps = episodesOf(release);
  const epRange =
    eps.length === 1
      ? `E${pad(eps[0])}`
      : eps.length > 1
        ? `E${pad(eps[0])}-E${pad(eps[eps.length - 1])}`
        : null;
  if (release.season != null && epRange) return `S${pad(release.season)}${epRange}`;
  if (release.season != null) return `S${pad(release.season)}`;
  return epRange;
}

export function seasonEpisodeChip(release: Release): { text: string; pack: boolean } | null {
  if (mediaTypeOf(release) === "movie") return null;
  const season = release.season != null ? `第${release.season}季` : null;
  const eps = episodesOf(release);
  const episode = eps.length > 0 ? `第${formatNumList(eps)}集` : null;
  if (!season && !episode) return null;
  const pack = release.season != null && release.episode == null;
  return { text: [season, episode].filter(Boolean).join(" · "), pack };
}

export function specSummary(release: Release): string | null {
  const parts = [
    seasonEpLabel(release),
    release.resolution,
    release.source,
    release.codec,
    release.hdr,
    release.group,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
}

export function torrentPromoBadges(
  hit: TorrentHit,
  variant: "row" | "poster",
): { text: string; cls: string }[] {
  const badges: { text: string; cls: string }[] = [];
  if (hit.free) {
    badges.push({
      text: "免费",
      cls: variant === "poster" ? "bg-[var(--ok)]/90 text-[#052e16]" : "bg-[var(--ok)]/15 text-[#79d193]",
    });
  }
  if (hit.hr) {
    badges.push({
      text: "H&R",
      cls: variant === "poster" ? "bg-[#f59e0b]/90 text-[#3a2600]" : "bg-[#f59e0b]/15 text-[#fbbf24]",
    });
  }
  return badges;
}
