import type { LibraryKind } from "@/lib/media-types";

import type { LibraryItem } from "./item-types";
import type {
  LibraryAccessMode,
  LibraryOperation,
  LibraryRemoval,
  MatchRule,
  MediaLibrary,
  MetadataRefreshReceipt,
  RootReassignment,
} from "./types";

/** 后端统一响应信封（自有 API：{ ok, data }，见 docs/api-contracts/self.md） */
export interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

/** 列表信封上的分页元数据（`data` 仍是数组，游标/总数挂在信封上）。 */
export interface PageEnvelopeMeta {
  next_cursor?: string | null;
  total?: number | null;
}

export async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/** 列表端点只接受数组载荷；其它形状是契约破坏，直接报错而不是渲染空态。 */
export function arrayPayload(raw: unknown, label: string): unknown[] {
  if (!Array.isArray(raw)) throw new Error(`${label}响应格式错误`);
  return raw;
}

export function str(v: unknown): string {
  return v == null ? "" : String(v);
}

export function num(v: unknown): number {
  const n = Number(v);
  return Number.isFinite(n) ? n : 0;
}

export function bool(v: unknown): boolean {
  return Boolean(v);
}

export function emptyLibrary(
  partial: { name?: string; kind?: LibraryKind; root_paths?: string[] } = {},
): MediaLibrary {
  return {
    id: "",
    name: partial.name ?? "",
    kind: partial.kind ?? "movie",
    root_paths: partial.root_paths ?? [],
    root_missing: false,
    is_default: false,
    access_mode: "everyone",
    admin_visible: true,
    member_ids: [],
    stats: { item_count: 0, file_count: 0, total_size_bytes: 0 },
  };
}

/**
 * 从 PATCH 响应里认出「这是 202 受理的操作」而不是库详情。
 *
 * 判别只认 `operation_id`（同步更新返回的库投影没有这个字段），避免把库当成操作。
 */
export function libraryOperationFrom(raw: unknown): LibraryOperation | null {
  const r = (raw ?? {}) as Record<string, unknown>;
  const operationId = typeof r.operation_id === "string" && r.operation_id ? r.operation_id : null;
  if (!operationId) return null;
  if (typeof r.state !== "string" || !r.state) {
    throw new Error("Library operation response is missing state");
  }
  const state = r.state;
  return {
    operation_id: operationId,
    kind: typeof r.kind === "string" ? r.kind : "roots",
    state,
    cursor: typeof r.cursor === "string" ? r.cursor : null,
    error: typeof r.error === "string" ? r.error : null,
  };
}

export function libraryFrom(raw: unknown): MediaLibrary {
  const r = (raw ?? {}) as Record<string, unknown>;
  const stats = (r.stats ?? {}) as Record<string, unknown>;
  return {
    id: str(r.id),
    name: str(r.name),
    kind: (r.kind as LibraryKind) ?? "movie",
    root_paths: Array.isArray(r.root_paths) ? (r.root_paths as string[]) : [],
    root_missing: r.root_missing == null ? false : bool(r.root_missing),
    removal: r.removal == null ? null : (r.removal as LibraryRemoval),
    root_reassignments: Array.isArray(r.root_reassignments)
      ? (r.root_reassignments as RootReassignment[])
      : [],
    is_default: bool(r.is_default),
    access_mode: (r.access_mode as LibraryAccessMode) ?? "everyone",
    admin_visible: r.admin_visible == null ? true : bool(r.admin_visible),
    member_ids: Array.isArray(r.member_ids) ? (r.member_ids as string[]) : [],
    match_rules: Array.isArray(r.match_rules) ? (r.match_rules as MatchRule[]) : [],
    default_filter_id: r.default_filter_id != null ? str(r.default_filter_id) : null,
    detect_intros: r.detect_intros == null ? false : bool(r.detect_intros),
    enable_fingerprint: r.enable_fingerprint == null ? false : bool(r.enable_fingerprint),
    realtime_watch: r.realtime_watch == null ? true : bool(r.realtime_watch),
    generate_thumbnails: r.generate_thumbnails == null ? true : bool(r.generate_thumbnails),
    extract_chapter_images: r.extract_chapter_images == null ? true : bool(r.extract_chapter_images),
    exclude_from_home: r.exclude_from_home == null ? false : bool(r.exclude_from_home),
    auto_series_collections:
      r.auto_series_collections == null ? true : bool(r.auto_series_collections),
    stats: {
      item_count: num(stats.item_count),
      file_count: num(stats.file_count),
      strm_file_count: num(stats.strm_file_count),
      total_size_bytes: num(stats.total_size_bytes),
      refreshed_at_ms: num(stats.refreshed_at_ms),
      stale: stats.stale == null ? false : bool(stats.stale),
    },
  };
}

export function itemFrom(raw: unknown): LibraryItem {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    media_item_id: str(r.media_item_id),
    kind: (r.kind as LibraryKind) ?? "movie",
    library_id: str(r.library_id),
    title: str(r.title),
    year: r.year == null ? null : num(r.year),
    rating: r.rating == null ? null : num(r.rating),
    studio: r.studio == null ? null : str(r.studio),
    studio_logo: r.studio_logo == null ? null : str(r.studio_logo),
    poster_url: r.poster_url == null ? null : str(r.poster_url),
    backdrop_url: r.backdrop_url == null ? null : str(r.backdrop_url),
    file_count: num(r.file_count),
    seasons: Array.isArray(r.seasons) ? (r.seasons as number[]) : [],
    episode_count: num(r.episode_count),
    missing: r.missing == null ? false : bool(r.missing),
  };
}

export function refreshReceiptOf(data: Record<string, unknown>): MetadataRefreshReceipt {
  return { refreshed: num(data.refreshed) };
}
