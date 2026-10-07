import { cachedImageUrl } from "@/lib/image-proxy";
import { request } from "@/lib/http";
import {
  normalizeDiscoverySectionError,
  shouldCacheDiscoverySection,
  type SectionErrorInfo,
} from "@/lib/discovery-section-state";
import type { DiscoveryFilters } from "@/lib/discovery-filters";
import type {
  MediaLibraryLink,
  MediaLibraryStatus,
  MediaItem,
  MediaRowData,
  MediaSource,
  MediaType,
} from "@/lib/media-types";

// ---------------------------------------------------------------------------
// Discover 领域 DTO（snake_case）→ 前端渲染模型（camelCase）
// ---------------------------------------------------------------------------
// 自有 API 已删除 `/ui/discovery/{kind}`、`/discover/titles` 等端点；影视条目
// 搜索统一走 `/search/titles?keyword=`（见 lib/api/search.ts）。本模块保留
// 组件引用的导出签名与 DTO 转换函数（search.ts 复用），其余拉取函数返回空数据。

interface MediaLibraryStatusDto {
  media_item_id: number;
  library_count: number;
  file_count: number;
}

export interface DiscoveredTitleDto {
  title_ref: string;
  provider: MediaSource;
  external_id: string;
  media_type: MediaType | null;
  title: string;
  original_title: string;
  release_year: number | null;
  provider_rating: number;
  genres: string[];
  extent_label: string;
  overview: string;
  poster_url: string;
  backdrop_url: string | null;
  library_status?: MediaLibraryStatusDto | null;
}

export interface TitleSearchDto {
  query: string;
  titles: DiscoveredTitleDto[];
  providers: TitleSearchProviderStatusDto[];
  history_id: number | null;
}

interface TitleSearchProviderStatusDto {
  provider: MediaSource;
  success: boolean;
  result_count: number;
  message: string | null;
}

export interface MediaSearchItemDto {
  id: string;
  source: MediaSource;
  title: string;
  year?: number | null;
  type?: MediaType | null;
  rating: number;
  poster_url: string;
}

// ---------------------------------------------------------------------------
// 前端渲染模型（组件引用；旧发现页编排字段保留以兼容导入面）
// ---------------------------------------------------------------------------

export type DiscoveryPresentation = "hero" | "ranked-row" | "poster-row";

export interface DiscoveryPageSection {
  collectionRef: string;
  title: string;
  presentation: DiscoveryPresentation;
  previewLimit: number;
  supportsFullListing: boolean;
  error?: SectionErrorInfo;
}

export interface DiscoveryPageData {
  provider: MediaSource;
  mediaType: MediaType;
  sections: DiscoveryPageSection[];
  /** 后端 TMDB API key 是否已配置;未配置时全源合并只显示豆瓣,前端提示去配置 */
  tmdbConfigured?: boolean;
}

export interface DiscoveredCollectionData {
  collectionRef: string;
  provider: MediaSource;
  mediaType: MediaType;
  name: string;
  isRanked: boolean;
  supportsFullListing: boolean;
  items: MediaItem[];
  returnedCount: number;
  truncated: boolean;
  page: number;
  totalPages: number;
  totalResults: number;
  hasMore: boolean;
}

export interface DiscoveryGenre {
  id: number;
  name: string;
}

export interface FilteredDiscoveryData {
  mediaType: MediaType;
  items: MediaItem[];
  page: number;
  totalPages: number;
  totalResults: number;
  hasMore: boolean;
}

export interface MediaSearchItem {
  /** 下一步读取详情时原样传回的稳定引用。旧历史快照没有该字段。 */
  titleRef?: string;
  id: string;
  source: MediaSource;
  title: string;
  year?: number;
  type?: MediaType;
  rating: number;
  posterUrl: string;
}

export interface TitleSearchProviderStatus {
  provider: MediaSource;
  success: boolean;
  resultCount: number;
  message?: string;
}

export interface TitleSearchData {
  items: MediaSearchItem[];
  providers: TitleSearchProviderStatus[];
  historyId?: number;
}

export interface MediaCastMember {
  name: string;
  role?: string;
  avatarUrl?: string;
  tmdbPersonId?: number;
}

export interface MediaDetailInfo {
  directors: string[];
  directorCredits: MediaCastMember[];
  cast: MediaCastMember[];
  country: string;
  language: string;
  released: string;
  network?: string;
  aliases: string[];
  sourceUrl?: string;
}

export interface MediaImage {
  previewUrl: string;
  fullUrl: string;
  width: number;
  height: number;
}

/** 一段预告片；播放地址由后端拼好，前端只负责内嵌与外链两种打开方式。 */
export interface MediaVideo {
  key: string;
  name: string;
  kind: string;
  thumbnailUrl: string;
  embedUrl: string;
  watchUrl: string;
}

export interface MediaSeasonOverview {
  season_number: number;
  name?: string | null;
  episode_count?: number | null;
  air_date?: string | null;
}

export interface MediaEpisodeDetail {
  episode_number: number;
  name?: string | null;
  overview?: string | null;
  still_url?: string | null;
  air_date?: string | null;
}

export interface MediaDetailData {
  item: MediaItem;
  info: MediaDetailInfo;
  videos: MediaVideo[];
  backdrops: MediaImage[];
  posters: MediaImage[];
  backdropOriginalUrl?: string;
  collection?: { id: string; name: string; items: MediaItem[] };
  related: MediaItem[];
  libraryLinks: MediaLibraryLink[];
  seasons?: MediaSeasonOverview[];
}

export interface DiscoveredPersonDetailsData {
  tmdbPersonId: number;
  name: string;
  avatarUrl: string;
  items: MediaItem[];
  /** 分页：TMDB combined_credits 一次拿全，这里按页返回。 */
  page: number;
  totalPages: number;
  totalResults: number;
  hasMore: boolean;
}

// ---------------------------------------------------------------------------
// DTO → 渲染模型转换（search.ts 复用，保持原样）
// ---------------------------------------------------------------------------

function toLibraryStatus(dto: MediaLibraryStatusDto | null | undefined): MediaLibraryStatus | null {
  if (!dto) return null;
  return {
    mediaItemId: dto.media_item_id,
    libraryCount: dto.library_count,
    fileCount: dto.file_count,
  };
}

/** 片单与详情中的条目字段完整；空值兜底只覆盖上游偶发缺失，不猜来源身份。 */
function toItem(dto: DiscoveredTitleDto): MediaItem {
  return {
    titleRef: dto.title_ref,
    id: dto.external_id,
    source: dto.provider,
    type: dto.media_type ?? "movie",
    title: dto.title,
    originalTitle: dto.original_title,
    year: dto.release_year ?? 0,
    rating: dto.provider_rating,
    genres: dto.genres,
    extent: dto.extent_label,
    badges: [],
    overview: dto.overview,
    posterUrl: cachedImageUrl(dto.poster_url),
    backdropUrl: dto.backdrop_url ? cachedImageUrl(dto.backdrop_url) : undefined,
    libraryStatus: toLibraryStatus(dto.library_status),
  };
}

export function toDiscoveredSearchItem(dto: DiscoveredTitleDto): MediaSearchItem {
  return {
    titleRef: dto.title_ref,
    id: dto.external_id,
    source: dto.provider,
    title: dto.title,
    year: dto.release_year ?? undefined,
    type: dto.media_type ?? undefined,
    rating: dto.provider_rating,
    posterUrl: cachedImageUrl(dto.poster_url),
  };
}

/** 旧媒体搜索快照 DTO → 前端视图；快照在迁移前已落库，因此继续兼容。 */
export function toSearchItem(item: MediaSearchItemDto): MediaSearchItem {
  return {
    id: item.id,
    source: item.source,
    title: item.title,
    year: item.year ?? undefined,
    type: item.type ?? undefined,
    rating: item.rating,
    posterUrl: cachedImageUrl(item.poster_url),
  };
}

/** 从旧页面路由参数构造稳定引用；新的接口调用只消费此引用。 */
export function titleRef(source: MediaSource, type: MediaType, id: string): string {
  if (source === "douban") return `douban:${id}`;
  if (source === "tmdb") return `tmdb:${type}:${id}`;
  return `${source}:${type}:${id}`;
}

/** 把稳定片单引用转换为通用「看全部」页面地址。 */
export function collectionHref(collectionRef: string): string | undefined {
  const [provider, mediaType, ...idParts] = collectionRef.split(":");
  const collectionId = idParts.join(":");
  if (
    (provider !== "tmdb" && provider !== "douban") ||
    (mediaType !== "movie" && mediaType !== "tv") ||
    !collectionId
  ) {
    return undefined;
  }
  return `/discover/${mediaType}/collections/${provider}/${encodeURIComponent(collectionId)}`;
}

/** 横滚行组件仍消费 MediaRowData，此转换只属于前端展示层。 */
export function collectionToRow(collection: DiscoveredCollectionData): MediaRowData {
  return {
    id: collection.collectionRef,
    title: collection.name,
    items: collection.items,
  };
}

interface DiscoverEnvelope {
  ok: boolean;
  data: {
    kind: MediaType;
    tmdb_configured?: boolean;
    sections: Array<{
      id: string;
      title: string;
      presentation?: "hero" | "ranked-row" | "poster-row";
      supports_full_listing?: boolean;
      error?: { code?: string; message?: string } | null;
      items: Array<{
        id: string;
        title: string;
        year?: number | null;
        kind: MediaType;
        tmdb_id?: string | null;
        poster_url?: string | null;
        backdrop_url?: string | null;
        rating?: number | null;
        overview?: string | null;
      }>;
    }>;
  };
}

const popularCache = new Map<string, DiscoveredCollectionData>();

function toMediaItem(
  mediaType: MediaType,
  row: DiscoverEnvelope["data"]["sections"][0]["items"][0],
  provider: MediaSource = "tmdb",
): MediaItem {
  const isDouban = provider === "douban";
  const ref = isDouban
    ? `douban:${row.id}`
    : provider === "tmdb"
      ? (row.tmdb_id ? `tmdb:${mediaType}:${row.tmdb_id}` : undefined)
      : `${provider}:${mediaType}:${row.id}`;
  return {
    titleRef: ref,
    id: row.id,
    source: provider,
    type: mediaType,
    title: row.title,
    originalTitle: row.title,
    year: row.year ?? 0,
    rating: row.rating ?? 0,
    genres: [],
    extent: "",
    badges: [],
    overview: row.overview ?? "",
    posterUrl: cachedImageUrl(row.poster_url ?? ""),
    backdropUrl: row.backdrop_url ? cachedImageUrl(row.backdrop_url) : undefined,
  };
}

/** 热门编排：GET /discover/{movie|tv}（可选 ?source=tmdb|douban）。 */
export async function fetchDiscoveryPage(
  mediaType: MediaType,
  provider: MediaSource = "tmdb",
  init?: RequestInit,
): Promise<DiscoveryPageData> {
  const suffix = provider !== "tmdb" ? `?source=${provider}` : "";
  const envelope = await request<DiscoverEnvelope>(`/discover/${mediaType}${suffix}`, init);
  const sections = (envelope.data.sections ?? []).map((section, index) => {
    const mapped = (section.items ?? []).map((row) => toMediaItem(mediaType, row, provider));
    const collectionRef = `${provider}:${mediaType}:${section.id}`;
    const supportsFullListing = section.supports_full_listing === true;
    const sectionError = normalizeDiscoverySectionError(section.error);
    if (shouldCacheDiscoverySection(sectionError)) {
      popularCache.set(collectionRef, {
        collectionRef,
        provider,
        mediaType,
        name: section.title,
        isRanked: section.id === "top_rated",
        supportsFullListing,
        items: mapped,
        returnedCount: mapped.length,
        truncated: false,
        page: 1,
        totalPages: 1,
        totalResults: mapped.length,
        hasMore: false,
      });
    } else {
      popularCache.delete(collectionRef);
    }
    // Hero 需要宽幅 backdrop（TMDB 有，豆瓣只有竖版海报）——豆瓣源的首行
    // 虽标 hero 但没有 backdrop 图，降级为海报行，避免黑屏轮播。若该行加载失败，
    // 必须保留 hero 角色以便触发 Hero 专属错误警示与重试。
    const presentation: DiscoveryPresentation =
      section.presentation === "hero" &&
      index === 0 &&
      (Boolean(sectionError) || mapped.some((item) => item.backdropUrl))
        ? "hero"
        : "poster-row";
    return {
      collectionRef,
      title: section.title,
      presentation,
      previewLimit: Math.max(mapped.length, 20),
      supportsFullListing,
      error: sectionError,
    } satisfies DiscoveryPageSection;
  });
  return { provider, mediaType, sections, tmdbConfigured: envelope.data.tmdb_configured };
}

import {
  type DiscoveryCollectionMode,
  shouldUsePreviewCache,
} from "@/lib/discovery-collection-cache";

export type { DiscoveryCollectionMode };

/** 浏览一个片单；热门行走 fetchDiscoveryPage 写入的缓存。 */
export async function browseDiscoveryCollection(
  collectionRef: string,
  limit: number,
  init?: RequestInit,
  page = 1,
  mode: DiscoveryCollectionMode = "preview",
): Promise<DiscoveredCollectionData> {
  void limit;
  if (shouldUsePreviewCache(mode, page)) {
    const cached = popularCache.get(collectionRef);
    if (cached) return cached;
  }
  // collectionRef: tmdb:movie:popular | tmdb:tv:top-rated | douban:movie:top-rated
  const [provider, mediaType, id] = collectionRef.split(":");
  const kind: MediaType = mediaType === "tv" ? "tv" : "movie";
  const sourceSuffix = provider === "douban" ? "&source=douban" : "";
  const envelope = await request<CollectionEnvelope>(
    `/discover/${kind}/collection/${id}?page=${page}&page_size=20${sourceSuffix}`,
    init,
  );
  const data = envelope.data;
  return {
    collectionRef,
    provider: provider === "douban" ? "douban" : "tmdb",
    mediaType: kind,
    name: data.title ?? data.collection_id,
    isRanked: data.collection_id === "top-rated",
    supportsFullListing: true,
    items: (data.items ?? []).map((row) => toMediaItem(kind, row, provider as MediaSource)),
    returnedCount: (data.items ?? []).length,
    truncated: false,
    page: data.page ?? page,
    totalPages: data.total_pages ?? 0,
    totalResults: data.total_results ?? 0,
    hasMore: data.has_more === true,
  };
}

interface CollectionEnvelope {
  ok: boolean;
  data: {
    kind: MediaType;
    collection_id: string;
    title?: string;
    items: Array<{
      id: string;
      title: string;
      year?: number | null;
      kind: MediaType;
      tmdb_id?: string | null;
      poster_url?: string | null;
      backdrop_url?: string | null;
      rating?: number | null;
      overview?: string | null;
    }>;
    page: number;
    total_pages: number;
    total_results: number;
    has_more: boolean;
  };
}

/** 类型列表：GET /genres/{movie|tv}（TMDB genre 表）。 */
export async function fetchDiscoveryGenres(
  mediaType: MediaType,
  init?: RequestInit,
): Promise<DiscoveryGenre[]> {
  const envelope = await request<GenreEnvelope>(`/genres/${mediaType}`, init);
  return envelope.data.genres ?? [];
}

interface GenreEnvelope {
  ok: boolean;
  data: { kind: MediaType; genres: DiscoveryGenre[] };
}

/** 六维筛选：GET /discover/{kind}/filtered（TMDB discover）。 */
export async function fetchFilteredDiscovery(
  mediaType: MediaType,
  filters: DiscoveryFilters,
  page: number,
  init?: RequestInit,
): Promise<FilteredDiscoveryData> {
  const query = new URLSearchParams();
  if (filters.genreIds.length > 0) query.set("genres", filters.genreIds.join(","));
  if (filters.originCountry) query.set("country", filters.originCountry);
  if (filters.year) query.set("year", String(filters.year));
  if (filters.ratingGte !== undefined) query.set("rating", String(filters.ratingGte));
  if (filters.runtimeLte !== undefined) query.set("runtime", String(filters.runtimeLte));
  query.set("sort", filters.sort);
  query.set("page", String(page));
  const suffix = `?${query}`;
  const envelope = await request<FilteredEnvelope>(`/discover/${mediaType}/filtered${suffix}`, init);
  const items = (envelope.data.items ?? []).map((row) => toMediaItem(mediaType, row));
  const totalPages = envelope.data.total_pages ?? (items.length > 0 ? 1 : 0);
  const totalResults = envelope.data.total_results ?? items.length;
  const hasMore = envelope.data.has_more ?? (page < totalPages);
  return {
    mediaType,
    items,
    page: envelope.data.page ?? page,
    totalPages,
    totalResults,
    hasMore,
  };
}

interface FilteredEnvelope {
  ok: boolean;
  data: {
    kind: MediaType;
    page?: number;
    total_pages?: number;
    total_results?: number;
    has_more?: boolean;
    items: Array<{
      id: string;
      title: string;
      year?: number | null;
      kind: MediaType;
      tmdb_id?: string | null;
      poster_url?: string | null;
    }>;
  };
}

/** 读取一个稳定影视引用的详情（GET /media/{kind}/{id}）。 */
export async function fetchDiscoveredTitleDetails(
  reference: string,
  init?: RequestInit,
): Promise<MediaDetailData> {
  // reference 形如 tmdb:movie:603、douban:1291843 或纯数字 TMDB id。
  const parts = reference.split(":");
  // douban: 前缀 → 豆瓣详情端点（豆瓣 id 不带 movie/tv，后端探测 kind）。
  if (parts[0] === "douban") {
    const id = parts[parts.length - 1];
    const envelope = await request<MediaDetailEnvelope>(`/media/douban/${id}`, init);
    const kind = (envelope.data.item.kind === "tv" ? "tv" : "movie") as MediaType;
    return toMediaDetailData(kind, envelope.data, "douban");
  }
  const kind = (parts[1] === "tv" ? "tv" : "movie") as MediaType;
  const id = parts[parts.length - 1];
  const envelope = await request<MediaDetailEnvelope>(`/media/${kind}/${id}`, init);
  return toMediaDetailData(kind, envelope.data, "tmdb");
}

interface MediaDetailEnvelope {
  ok: boolean;
  data: {
    item: {
      id: string;
      title: string;
      year?: number | null;
      kind: MediaType;
      tmdb_id?: string | null;
      poster_url?: string | null;
      backdrop_url?: string | null;
      rating?: number | null;
    };
    info: {
      overview?: string | null;
      rating?: string | null;
      runtime?: string | null;
      genres?: string[];
      cast?: Array<{ name: string; role?: string | null; tmdb_person_id?: number | null; avatar_url?: string | null }>;
      directors?: string[];
      director_credits?: Array<{ name: string; role?: string | null }>;
      country?: string;
      language?: string;
      released?: string;
      aliases?: string[];
    };
    videos?: Array<Record<string, unknown>>;
    backdrops?: Array<{ preview_url: string; full_url: string; width: number; height: number }>;
    posters?: Array<{ preview_url: string; full_url: string; width: number; height: number }>;
    backdrop_original_url?: string | null;
    related?: Array<{
      id: string;
      title: string;
      year?: number | null;
      kind: MediaType;
      tmdb_id?: string | null;
      poster_url?: string | null;
      rating?: number | null;
    }>;
    library_links?: Array<{
      library_id: string;
      library_name: string;
      media_item_id: string;
    }>;
    seasons?: Array<{
      season_number: number;
      name?: string | null;
      episode_count?: number | null;
      air_date?: string | null;
    }>;
  };
}

function toMediaDetailData(
  mediaType: MediaType,
  data: MediaDetailEnvelope["data"],
  source: MediaSource = "tmdb",
): MediaDetailData {
  const item: MediaItem = {
    id: data.item.id,
    source,
    type: data.item.kind ?? mediaType,
    title: data.item.title || "",
    originalTitle: data.item.title || "",
    year: data.item.year ?? 0,
    rating: Number(data.info.rating ?? data.item.rating ?? 0) || 0,
    genres: data.info.genres ?? [],
    extent: "",
    badges: [],
    overview: data.info.overview ?? "",
    posterUrl: cachedImageUrl(data.item.poster_url ?? ""),
    backdropUrl: data.item.backdrop_url ? cachedImageUrl(data.item.backdrop_url) : undefined,
  };
  const cast = (data.info.cast ?? []).map((c) => ({
    name: c.name,
    role: c.role ?? undefined,
    avatarUrl: c.avatar_url ? cachedImageUrl(c.avatar_url) : undefined,
    tmdbPersonId: c.tmdb_person_id ?? undefined,
  }));
  const backdrops = (data.backdrops ?? []).map((b) => ({
    previewUrl: b.preview_url,
    fullUrl: b.full_url,
    width: b.width,
    height: b.height,
  }));
  const posters = (data.posters ?? []).map((p) => ({
    previewUrl: p.preview_url,
    fullUrl: p.full_url,
    width: p.width,
    height: p.height,
  }));
  const related = (data.related ?? []).map((r) => ({
    id: r.id,
    source: "tmdb" as const,
    type: r.kind ?? mediaType,
    title: r.title,
    originalTitle: r.title,
    year: r.year ?? 0,
    rating: r.rating ?? 0,
    genres: [],
    extent: "",
    badges: [],
    overview: "",
    posterUrl: cachedImageUrl(r.poster_url ?? ""),
  }));
  return {
    item,
    info: {
      directors: data.info.directors ?? [],
      directorCredits: (data.info.director_credits ?? []).map((c) => ({
        name: c.name,
        role: c.role ?? undefined,
      })),
      cast,
      country: data.info.country ?? "",
      language: data.info.language ?? "",
      released: data.info.released ?? "",
      aliases: data.info.aliases ?? [],
    },
    videos: [],
    backdrops,
    posters,
    backdropOriginalUrl: data.backdrop_original_url ?? undefined,
    related,
    libraryLinks: (data.library_links ?? []).map((l) => ({
      libraryId: l.library_id,
      libraryName: l.library_name,
      mediaItemId: l.media_item_id,
    })),
    seasons: data.seasons,
  };
}

/** 读取发现页影人的完整 TMDB 影视履历（GET /media/person/{id}）。 */
export async function fetchDiscoveredPersonDetails(
  tmdbPersonId: number | string,
  init?: RequestInit,
  page = 1,
): Promise<DiscoveredPersonDetailsData> {
  const envelope = await request<PersonDetailEnvelope>(
    `/media/person/${tmdbPersonId}?page=${page}&page_size=40`,
    init,
  );
  const data = envelope.data;
  return {
    tmdbPersonId: Number(tmdbPersonId),
    name: data.name,
    avatarUrl: data.avatar_url ?? "",
    items: (data.items ?? []).map((row) => ({
      id: row.id,
      source: "tmdb" as const,
      type: (row.kind === "tv" ? "tv" : "movie") as MediaType,
      title: row.title,
      originalTitle: row.title,
      year: row.year ?? 0,
      rating: 0,
      genres: [],
      extent: "",
      badges: [],
      overview: "",
      posterUrl: cachedImageUrl(row.poster_url ?? ""),
      titleRef: row.tmdb_id ? `tmdb:${row.kind === "tv" ? "tv" : "movie"}:${row.tmdb_id}` : undefined,
    })),
    page: data.page ?? page,
    totalPages: data.total_pages ?? 0,
    totalResults: data.total_results ?? 0,
    hasMore: data.has_more === true,
  };
}

interface PersonDetailEnvelope {
  ok: boolean;
  data: {
    tmdb_person_id: string;
    name: string;
    avatar_url?: string | null;
    items: Array<{
      id: string;
      title: string;
      year?: number | null;
      kind: MediaType;
      tmdb_id?: string | null;
      poster_url?: string | null;
      rating?: number | null;
    }>;
    page: number;
    total_pages: number;
    total_results: number;
    has_more: boolean;
  };
}
