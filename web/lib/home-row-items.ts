/**
 * 首页「库行」向服务端要哪一份条目：取数参数与缓存键。
 *
 * 行 → 请求的映射收在这一处，因为两件事都很容易悄悄错：
 *
 * 1. **「未看优先」的口径在后端**（`library::WatchTier`）。库行带
 *    `unwatched_first=true`：服务端把观看分级排进顺序——没开始看过的在前、在看其次、
 *    已看完沉底，**段内仍是这一行自己的排序**（默认档就是最近入库倒序）。它只排不筛：
 *    一个只有 3 部剧、其中 2 部看过的小库，筛完只剩 1 张卡，看起来像坏了。
 *    「我的收藏」行用同一个开关、同一条规则。
 * 2. **取数键的形状**：库卡片封面（`coverFetchKey`）与默认库行（`rowFetchKey`）
 *    必须算出同一个键，否则同一份「最近入库」要打两次请求。
 *
 * 「最近观看」行是例外：它要的就是播过的片（`w=seen`），没播过就该是空的，
 * 也不参与未看优先——与 home-rows.ts 里「以排序为准，开关作废」同一条规矩。
 *
 * 本模块保持零 `@/` 依赖、纯函数：node --test 直接测（test/home-row-items.test.mjs）。
 */
import { type HomeRow, orderParamFor, SORT_PRESETS } from "./home-rows.ts";

/** 首页的行里跟取数有关的那一支。 */
type LibraryRow = Extract<HomeRow, { kind: "library" }>;

/** 一行取数的缓存键：同一个库同一种排序同一个方向同一个开关只请求一次。 */
export function rowFetchKey(row: HomeRow): string {
  if (row.kind === "library")
    return `lib:${row.library.id}:${row.sort}:${row.reversed}:${row.unwatched}`;
  if (row.kind === "collection") return `col:${row.collection.id}:${row.sort}:${row.reversed}`;
  return row.id;
}

/** 库卡片封面用的那批条目：最近入账的前几部，与默认的「最近添加」行共用一份。
 *  键的形状必须与 rowFetchKey 对库行算出来的一致，否则默认行会多打一次同样的请求。 */
export function coverFetchKey(libraryId: string): string {
  return `lib:${libraryId}:added_at:false:false`;
}

/** `listLibraryItems` 的形状（不 import 那边的类型：那个文件带 `@/` 别名）。 */
export interface RowItemQuery {
  sort: LibraryRow["sort"];
  /** 方向；与自然方向一致就不带（服务端按自然方向排） */
  order?: "asc" | "desc";
  limit: number;
  filter?: { watch: "seen" };
  /** 未看优先：观看分级参与排序（口径在后端），只排不筛 */
  unwatchedFirst?: boolean;
}

/**
 * 这一行向 `/libraries/{id}/items` 要的参数。`limit` 由调用方给（首页每行多少格）。
 *
 * 「最近观看」行只要播过的：度量档会把没播过的沉底而不是排除，取 20 条时看过的
 * 排完就轮到没播过的，首页这一行不能这样（`w=seen`）。
 */
export function rowItemQuery(row: LibraryRow, limit: number): RowItemQuery {
  const query: RowItemQuery = {
    sort: row.sort,
    order: orderParamFor(SORT_PRESETS[row.sort].direction, row.reversed),
    limit,
    filter: row.sort === "last_played" ? { watch: "seen" } : undefined,
  };
  // 未看优先只管浏览档：要播过的片的那一行不参与（它自己就是"看过"的意思）
  if (row.unwatched && row.sort !== "last_played") query.unwatchedFirst = true;
  return query;
}
