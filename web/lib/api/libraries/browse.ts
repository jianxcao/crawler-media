import { request } from "@/lib/http";
import { filterQuery, type LibraryFilter } from "@/lib/library-filter";

import {
  type ApiEnvelope,
  type PageEnvelopeMeta,
  arrayPayload,
  itemFrom,
  num,
  str,
  unwrap,
} from "./helpers";
import type {
  BrowseQueryParams,
  FacetValue,
  LibraryFacets,
  LibraryGalleryGroup,
  LibraryGalleryPage,
  LibraryIndexEntry,
  LibraryItemOrder,
  LibraryItemPage,
  LibraryRelax,
} from "./item-types";

/**
 * 浏览列表的查询串。
 *
 * 分页只有 `cursor`：`offset` 已从契约里删除，后端对任何 `offset` 都回 400
 * （按 offset 逐行跳过、每页可能带 COUNT，越深越慢）。这里因此**绝不**写
 * `offset`——真正被删掉的东西不该留一个前端兜底。
 */
export function buildBrowseQuery(params: BrowseQueryParams = {}): URLSearchParams {
  const query = new URLSearchParams();
  if (params.sort) query.set("sort", params.sort);
  if (params.order) query.set("order", params.order);
  if (params.identity) query.set("identity", params.identity);
  if (params.limit !== undefined) query.set("limit", String(params.limit));
  if (params.cursor) query.set("cursor", params.cursor);
  filterQuery(params.filter, query);
  // 与 /playback/favorites 同一个参数名：两处的「未看优先」是同一条规则
  if (params.unwatchedFirst) query.set("unwatched_first", "true");
  return query;
}

/**
 * 库内媒体条目的库存聚合（单库海报墙数据源）。
 * GET /libraries/{id}/items → data: [{media_item_id, kind, library_id, title, year,
 * poster_url?, file_count, seasons, episode_count}]，信封另有 `next_cursor` / `total`
 */
export async function listLibraryItems(
  id: string,
  params?: BrowseQueryParams,
): Promise<LibraryItemPage> {
  const query = buildBrowseQuery(params);
  const suffix = query.size > 0 ? `?${query}` : "";
  // 不能过 unwrap：它只把 `data` 取出来，而游标与总数挂在信封上。
  const envelope = await request<ApiEnvelope<unknown[]> & PageEnvelopeMeta>(
    `/libraries/${id}/items${suffix}`,
  );
  return {
    items: arrayPayload(envelope.data, "媒体库条目").map(itemFrom),
    next_cursor: envelope.next_cursor ?? null,
    total: envelope.total ?? null,
  };
}

/** 海报墙的首字母索引（GET /libraries/{id}/item-index）。 */
export function listLibraryItemIndex(
  id: string,
  sort: "title" | "release_date" | "rating" = "title",
  filter?: LibraryFilter,
  order?: LibraryItemOrder,
  options?: { gallery?: boolean },
): Promise<LibraryIndexEntry[]> {
  const query = new URLSearchParams();
  if (sort !== "title") query.set("sort", sort);
  if (order) query.set("order", order);
  if (options?.gallery) query.set("gallery", "true");
  filterQuery(filter, query);
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<unknown[]>>(`/libraries/${id}/item-index${suffix}`)).then(
    (raw) =>
      arrayPayload(raw, "首字母索引").map((entry) => {
        const r = (entry ?? {}) as Record<string, unknown>;
        return {
          initial: str(r.initial),
          count: num(r.count),
          cursor: r.cursor == null ? null : str(r.cursor),
        };
      }),
  );
}

/** 筛空放宽建议（POST /libraries/{id}/items/relax-filter）。 */
export async function getLibraryRelax(id: string, filter?: LibraryFilter): Promise<LibraryRelax> {
  return unwrap(
    request<ApiEnvelope<LibraryRelax>>(`/libraries/${id}/items/relax-filter`, {
      method: "POST",
      body: JSON.stringify(filter ?? {}),
    }),
  );
}

/**
 * 筛选面板的候选值与计数（GET /libraries/{id}/facets）。
 */
export async function getLibraryFacets(
  id: string,
  filter?: LibraryFilter,
  tier: "primary" | "all" = "primary",
): Promise<LibraryFacets> {
  const query = new URLSearchParams();
  if (tier === "all") query.set("tier", "all");
  filterQuery(filter, query);
  const suffix = query.size > 0 ? `?${query}` : "";
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/libraries/${id}/facets${suffix}`));
  const r = (raw ?? {}) as Record<string, unknown>;
  const dim = (key: string): FacetValue[] =>
    Array.isArray(r[key]) ? (r[key] as FacetValue[]) : [];
  return {
    total: num(r.total),
    genres: dim("genres"),
    countries: dim("countries"),
    decades: dim("decades"),
    watch: dim("watch"),
    ratings: dim("ratings"),
    runtimes: dim("runtimes"),
    languages: dim("languages"),
    resolutions: dim("resolutions"),
    hdr: dim("hdr"),
    stock: dim("stock"),
  };
}

/** 图廊按条目浏览（GET /libraries/{id}/gallery：每部作品的海报/背景/分集剧照）。 */
export async function listLibraryGallery(
  id: string,
  params?: Omit<BrowseQueryParams, "identity" | "unwatchedFirst">,
): Promise<LibraryGalleryPage> {
  const query = buildBrowseQuery(params);
  const suffix = query.size > 0 ? `?${query}` : "";
  const envelope = await request<ApiEnvelope<LibraryGalleryGroup[]> & PageEnvelopeMeta>(
    `/libraries/${id}/gallery${suffix}`,
  );
  return { groups: envelope.data, next_cursor: envelope.next_cursor ?? null };
}
