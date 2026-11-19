import type { LibraryItemSort } from "@/lib/api/libraries";

import { filterQuery, type LibraryFilter, type WatchFilter } from "./library-filter.ts";
import type { WallSortPref } from "./wall-sort.ts";

/** 海报墙每次向服务端要的格数（首屏一批，滚到底再追加一批）。 */
export const WALL_PAGE_SIZE = 60;
/** 后端单次分页的硬上限；轮询已加载窗口时按此上限分块请求。 */
export const WALL_API_PAGE_SIZE = 200;

/**
 * 游标链尾还有没有下一页。
 *
 * 约定：`cursors[i]` 是第 i 页的起始游标（跳档后 0 号是该档的游标），
 * 每取完一页就把它的 `next_cursor` 写进 `cursors[i + 1]`。因此最后一个元素就是
 * 「下一页的游标」，null = 已经到底（或还没开始取，此时也当作没有更多）。
 */
export function hasMorePages(cursors: readonly (string | null)[]): boolean {
  return cursors.length > 1 && cursors[cursors.length - 1] !== null;
}

/**
 * 列表拉取失败折成 ``null``（而不是空数组）。
 *
 * 「拉不到」和「没有」是两回事，待办清单尤其不能混：折成空数组的话，一次
 * 500 / 超时 / 权限不足会让胶囊与计数静默归零，页面上找不到任何异常痕迹，
 * 用户只会以为待办都处理完了。调用方拿到 null 就保留上一份快照并点亮顶部
 * 的重试提示条——展示旧数据总好过展示假数据。
 */
export function keepOnError<T>(rows: Promise<T[]>): Promise<T[] | null> {
  return rows.catch(() => null);
}

/**
 * 排序档位与展示值。
 *
 * 去掉了「排序」这个前缀标签——前提是让值自述：默认档不叫「默认」，而是
 * 按库的形态叫「按标题」/「按时间」（本来就有 defaultSortLabel 这套叫法）。
 *
 * 图廊（图床浏览）与海报墙同一套档位、同一个方向：两面墙翻的是同一份名单，服务端
 * 也是同一条排序查询。
 */
export function sortOptions(
  defaultLabel: string,
  timeline: boolean,
): readonly (readonly [WallSortPref, string])[] {
  return [
    ["default", defaultLabel],
    // 一次导入的内容入账时间都挤在一起，所以这一档对"陆续往库里添东西"才有意义
    ["added_at", "最近添加"],
    // 影视库补上「按上映时间」：老片→新片 / 新片→老片 是正倒序最用得上的一档。
    // 其他库的默认档本来就是按时间，不重复摆
    ...(timeline ? [] : ([["release_date", "按上映时间"]] as [WallSortPref, string][])),
    ["rating", "按评分"],
    ["runtime", "按片长"],
    ["size", "按体积"],
    ["last_played", "最近观看"],
  ];
}

/**
 * 从地址栏读筛选条件。
 *
 * **URL 是筛选态的唯一事实源**，筛选不进 localStorage——上次筛的条件在下次
 * 打开时还在，是本类产品最经典的困惑来源（"我的片怎么少了一半"）。
 */
export function readFilterFromUrl(): LibraryFilter {
  if (typeof window === "undefined") return {};
  const params = new URLSearchParams(window.location.search);
  const list = (key: string) => (params.get(key) ?? "").split(",").filter(Boolean);
  const watch = params.get("w");
  const rating = params.get("rating_gte");
  const hdr = params.get("hdr");
  return {
    genres: list("g"),
    countries: list("c"),
    decades: list("d"),
    watch: watch ? (watch as WatchFilter) : null,
    ratingGte: rating !== null && Number.isFinite(Number(rating)) ? Number(rating) : null,
    runtimes: list("rt"),
    languages: list("lang"),
    resolutions: list("res"),
    hdr: hdr === null ? null : hdr === "true",
    stock: list("stock"),
  };
}

/** 把筛选条件写回地址栏（replaceState：筛选不该在浏览器历史里堆一串条目）。 */
export function writeFilterToUrl(filter: LibraryFilter): void {
  if (typeof window === "undefined") return;
  const params = new URLSearchParams(window.location.search);
  for (const key of ["g", "c", "d", "w", "rating_gte", "rt", "lang", "res", "hdr", "stock"]) {
    params.delete(key);
  }
  filterQuery(filter, params);
  const rest = params.toString();
  window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}`);
}

/**
 * 哪些排序要请求索引：按标题（A-Z 索引条）与按内容时间（其他库 / 图片库的月份时间线）。
 */
export const INDEXED_SORTS: Partial<Record<LibraryItemSort, "title" | "release_date">> = {
  title: "title",
  probing: "title",
  release_date: "release_date",
};
