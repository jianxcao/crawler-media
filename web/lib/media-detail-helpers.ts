/**
 * 详情页（MediaDetailView / LibraryItemDetailView）共用的纯逻辑计算。
 */

export const MOBILE_HERO_HEIGHT = "min(115vw, 62svh)";

export type HdArtworkState = "pending" | "ok" | "failed";

/**
 * 电影入库即完成：隐藏「订阅追踪 / 搜索资源」，由「在库」入口接手；
 * 但已订阅的在库电影仍保留状态管理键。剧集在库仍可能缺集或追新季，两个动作均保留。
 */
export function resolveMediaDetailOwnership({
  isMovie,
  libraryLinksCount,
  hasSubscription,
  canSubscribe,
  canSearch,
}: {
  isMovie: boolean;
  libraryLinksCount: number;
  hasSubscription: boolean;
  canSubscribe: boolean;
  canSearch: boolean;
}): {
  ownedMovie: boolean;
  showSubscribeButton: boolean;
  showSearchButton: boolean;
} {
  const ownedMovie = isMovie && libraryLinksCount > 0;
  return {
    ownedMovie,
    showSubscribeButton: canSubscribe && (hasSubscription || !ownedMovie),
    showSearchButton: canSearch && !ownedMovie,
  };
}

/** 合并导演/主创与演员列表；电影标「导演」，剧集标「主创」。 */
export function buildDetailPeopleList<T extends { name: string }>({
  isMovie,
  directorCredits,
  directors,
  cast,
}: {
  isMovie: boolean;
  directorCredits?: T[];
  directors?: string[];
  cast?: T[];
}): Array<(T | { name: string }) & { credit?: string }> {
  const credit = isMovie ? "导演" : "主创";
  const directorCast = directorCredits?.length
    ? directorCredits.map((director) => ({ ...director, credit }))
    : [...new Set(directors ?? [])].map((name) => ({ name, credit }));
  return [...directorCast, ...(cast ?? [])];
}

/**
 * Netflix 桌面端详情页滚动退场进度（0.000 ~ 1.000）：
 * 归一化区间取容器高度 75%（下限 320px），与 globals.css 的 --nf-hero-recede 对齐。
 */
export function computeHeroRecedeProgress(scrollTop: number, clientHeight: number): string {
  const range = Math.max(320, clientHeight * 0.75);
  return Math.min(1, Math.max(0, scrollTop / range)).toFixed(3);
}

/** 豆瓣不换全站沉浸背景（仅小尺寸海报）；TMDB 高清加载期间保持黑场，失败再兜底。 */
export function resolveImmersiveBackdropUrl({
  source,
  hdState,
  hdUrl,
  fallbackBackdrop,
}: {
  source: string;
  hdState: HdArtworkState;
  hdUrl?: string;
  fallbackBackdrop: string;
}): string {
  if (source === "douban") return "";
  if (hdState === "ok") return hdUrl ?? "";
  if (hdState === "failed") return fallbackBackdrop;
  return "";
}

/** 手机页内 Hero 为有限高度画面框，豆瓣条目也展示；高清就位优先高清图。 */
export function resolveMobileHeroSrc({
  hdState,
  hdUrl,
  fallbackBackdrop,
}: {
  hdState: HdArtworkState;
  hdUrl?: string;
  fallbackBackdrop: string;
}): string {
  return hdState === "ok" && hdUrl ? hdUrl : fallbackBackdrop;
}
