import { fetchEventSource } from "@microsoft/fetch-event-source";
import { getAuthToken, HttpError, redirectToLoginOn401, request, resolveRequestUrl } from "@/lib/http";
import { cachedImageUrl } from "@/lib/image-proxy";
import type { MediaSearchItem, TitleSearchData } from "@/lib/api/discover";
import type { SearchScope, SearchTab, TorrentCategory } from "@/lib/categories";
import type { MediaSource, MediaType } from "@/lib/media-types";
import { toMediaSearchItem } from "@/lib/search-history";

/** 后端统一响应信封（见 docs/api-contracts/self.md） */
interface ApiEnvelope<T> {
  success: boolean;
  code: string;
  message: string;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

// ---------------------------------------------------------------------------
// 种子搜索 DTO（docs/api-contracts/self.md：search）
// ---------------------------------------------------------------------------

/** 标题解析出的结构化属性（release::parse 输出，字段提取不到为 null）。 */
export interface Release {
  title: string;
  year: number | null;
  season: number | null;
  episode: number | null;
  episode_to: number | null;
  resolution: string | null;
  source: string | null;
  codec: string | null;
  hdr: string | null;
  group: string | null;
  confidence: string | null;
}

/** 搜索命中的单条种子。 */
export interface TorrentHit {
  site_id: string;
  site_name: string;
  /** 站点内种子唯一身份（下载/详情链接或 RSS guid 提取）；缺省回退 enclosure */
  id: string | null;
  title: string;
  enclosure: string;
  size_bytes: number | null;
  seeders: number | null;
  leechers: number | null;
  snatched: number | null;
  /** 发布/上传时间（站点原文或 RFC3339） */
  upload_time: string | null;
  /** 海报 URL（站点提供时） */
  poster_url?: string | null;
  free: boolean;
  /** H&R 考核：true=有考核，false=明确无考核 */
  hr: boolean;
  imdb_id: string | null;
  detail_url: string | null;
  /** 站点分类（Movies / TV / Anime…） */
  category: string | null;
  /** 标题解析出的结构化属性（见 Release） */
  release: Release;
}

/** 单个站点在本次搜索里的执行情况。 */
export interface SiteSearchStatus {
  site_id: string;
  site_name: string;
  count: number;
  /** 失败原因（可读中文）；成功为 null */
  error: string | null;
  /** 该站从发起到返回/失败的耗时（毫秒） */
  elapsed_ms: number | null;
}

/** 跨站聚合搜索结果（GET /search/torrents JSON 一次返回）。 */
export interface SearchResponse {
  keyword: string;
  total: number;
  items: TorrentHit[];
  sites: SiteSearchStatus[];
}

// ---------------------------------------------------------------------------
// 搜索历史与预设
// ---------------------------------------------------------------------------

/** 搜索历史的单条记录。 */
export interface SearchHistoryItem {
  id: string;
  query: string;
  provider: string | null;
  created_at: string;
}

/** 获取最近的搜索历史。limit 保留签名兼容；新后端一次返回全部。 */
export async function listSearchHistory(
  limit = 10,
  init?: RequestInit,
): Promise<SearchHistoryItem[]> {
  void limit;
  const view = await unwrap(
    request<ApiEnvelope<{ items: SearchHistoryItem[] }>>("/search/history", init),
  );
  return view.items;
}

/** 一次历史 PT 资源搜索保存的完整结果。 */
export interface TorrentSearchHistoryResults {
  vertical: "torrents";
  history_id: string;
  keyword: string;
  label: string | null;
  categories: TorrentCategory[];
  site_ids: string[];
  snapshot_at: string;
  total: number;
  elapsed_ms: number | null;
  items: TorrentHit[];
  sites: SiteSearchStatus[];
}

/** 读取某条种子搜索历史的结果快照（GET /search/history/{id}?vertical=torrents）。 */
export function getTorrentSearchHistoryResults(
  historyId: string,
  init?: RequestInit,
): Promise<TorrentSearchHistoryResults> {
  return unwrap(
    request<ApiEnvelope<TorrentSearchHistoryResults>>(
      `/search/history/${encodeURIComponent(historyId)}?vertical=torrents`,
      init,
    ),
  ).then((snap) => ({
    vertical: "torrents",
    history_id: String(historyId),
    keyword: snap.keyword ?? "",
    label: null,
    categories: [],
    site_ids: [],
    snapshot_at: snap.snapshot_at ?? "",
    total: snap.total ?? snap.items?.length ?? 0,
    elapsed_ms: snap.elapsed_ms ?? null,
    items: snap.items ?? [],
    sites: snap.sites ?? [],
  }));
}

/** 一次历史影视条目搜索保存并转换后的完整结果。 */
export interface TitleSearchHistoryResults {
  vertical: "titles";
  history_id: string;
  keyword: string;
  snapshot_at: string;
  total: number;
  items: MediaSearchItem[];
}

interface TitleSearchHistorySnapshotDto {
  keyword?: string | null;
  snapshot_at?: string | null;
  total?: number | null;
  items?: TitleHit[] | null;
}

/** 读取影视条目历史结果快照（GET /search/history/{id}?vertical=titles）。 */
export function getTitleSearchHistoryResults(
  historyId: string,
  init?: RequestInit,
): Promise<TitleSearchHistoryResults> {
  return unwrap(
    request<ApiEnvelope<TitleSearchHistorySnapshotDto>>(
      `/search/history/${encodeURIComponent(historyId)}?vertical=titles`,
      init,
    ),
  ).then((snap) => ({
    vertical: "titles",
    history_id: String(historyId),
    keyword: snap.keyword ?? "",
    snapshot_at: snap.snapshot_at ?? "",
    total: snap.total ?? snap.items?.length ?? 0,
    items: (snap.items ?? []).map(titleHitToItem),
  }));
}

/** 删除单条搜索历史（含结果快照）。 */
export function deleteSearchHistoryEntry(id: string): Promise<null> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/search/history/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  ).then(() => null);
}

/** 清空全部搜索历史（含结果快照）。 */
export function clearSearchHistory(): Promise<null> {
  return unwrap(
    request<ApiEnvelope<{ cleared: boolean }>>("/search/history", {
      method: "DELETE",
    }),
  ).then(() => null);
}

/** 资源搜索预设视图：内置分类与自定义站点组合的完整有序列表。 */
interface SearchPresetListView {
  presets: SearchTab[];
}

/** 列出资源搜索预设；含隐藏的内置分类。 */
export async function listSearchPresets(init?: RequestInit): Promise<SearchTab[]> {
  const view = await unwrap(
    request<ApiEnvelope<SearchPresetListView>>("/search/presets", init),
  );
  return view.presets;
}

/** 整体覆盖式保存资源搜索预设，返回后端规范化后的完整列表。 */
export async function updateSearchPresets(presets: SearchTab[]): Promise<SearchTab[]> {
  const view = await unwrap(
    request<ApiEnvelope<SearchPresetListView>>("/search/presets", {
      method: "PUT",
      body: JSON.stringify({ presets }),
    }),
  );
  return view.presets;
}

// ---------------------------------------------------------------------------
// 影视条目搜索（GET /search/titles?keyword=）
// ---------------------------------------------------------------------------

/** 单个影视条目标题命中（catalog fanout 输出）。 */
export interface TitleHit {
  provider: string;
  external_id: string;
  kind: MediaType;
  title: string;
  year: number | null;
  original_title: string | null;
  poster_url: string | null;
}

interface TitleSearchProviderStatusDto {
  provider: string;
  ok: boolean;
  count: number;
  message?: string | null;
}

interface TitleSearchDto {
  query: string;
  titles: TitleHit[];
  providers: TitleSearchProviderStatusDto[];
}

function titleHitToItem(hit: TitleHit): MediaSearchItem {
  return toMediaSearchItem(hit, cachedImageUrl);
}

/** 搜索影视条目（GET query，非 POST body）；每个来源的成功/失败状态独立返回。 */
export async function searchTitles(
  query: string,
  options?: { provider?: MediaSource | "all"; saveHistory?: boolean },
  init?: RequestInit,
): Promise<TitleSearchData> {
  const params = new URLSearchParams({ keyword: query });
  if (options?.provider && options.provider !== "all") {
    params.set("provider", options.provider);
  }
  const dto = await unwrap(
    request<ApiEnvelope<TitleSearchDto>>(`/search/titles?${params}`, init),
  );
  return {
    items: dto.titles.map(titleHitToItem),
    providers: dto.providers.map((status) => ({
      provider: status.provider as MediaSource,
      success: status.ok,
      resultCount: status.count,
      message: status.message ?? undefined,
    })),
    historyId: undefined,
  };
}

// ---------------------------------------------------------------------------
// 媒体库条目搜索（GET /search/library-items?keyword=）
// ---------------------------------------------------------------------------

/** 媒体库内条目命中的一行（ledger 行 + 所属媒体）。 */
export interface LibraryHit {
  media_item_id: string;
  title: string;
  kind: MediaType;
  path: string;
  season: number | null;
  episode: number | null;
}

/** 搜索本地全部可见媒体库中的已入库条目。 */
export function searchLibraryItems(keyword: string): Promise<LibraryHit[]> {
  const query = new URLSearchParams({ keyword });
  return unwrap(request<ApiEnvelope<LibraryHit[]>>(`/search/library-items?${query}`));
}

// ---------------------------------------------------------------------------
// 跨站种子搜索（JSON + SSE）
// ---------------------------------------------------------------------------

export interface SearchParams {
  keyword: string;
  /** 搜索范围（标签换算而来）；不传等同「全部」 */
  scope?: SearchScope;
  page?: number;
  /** 保存一次完整搜索的历史和结果快照；重试/分页请求应省略。 */
  saveHistory?: boolean;
}

/**
 * 跨「已启用」的站点（可由 scope.siteIds 圈定子集）并发搜索种子资源。
 * 单站失败不影响整体，其原因见 `sites[].error`。
 */
export function searchTorrents(
  { keyword, scope, page, saveHistory }: SearchParams,
  init?: RequestInit,
): Promise<SearchResponse> {
  return unwrap(
    request<ApiEnvelope<SearchResponse>>(
      `/search/torrents?${searchParamsOf({ keyword, scope, page, saveHistory })}`,
      init,
    ),
  );
}

function searchParamsOf({ keyword, scope, page, saveHistory }: SearchParams): URLSearchParams {
  const params = new URLSearchParams({ keyword });
  for (const s of scope?.siteIds ?? []) params.append("sites", s);
  for (const category of scope?.categories ?? []) params.append("categories", category);
  if (page != null) params.set("page", String(page));
  if (scope?.skipHistory) params.set("skip_history", "true");
  if (saveHistory && !scope?.skipHistory) params.set("save_history", "true");
  return params;
}

/* —— SSE 流式搜索（GET /search/torrents/stream） —— */

/** `start` 事件的站点清单项 / `site_start` 事件的载荷。 */
export interface SearchStreamSite {
  site_id: string;
  site_name: string;
}

/** `start` 事件：宣告本次搜索的范围与参与站点。 */
export interface SearchStreamStart {
  keyword: string;
  sites: SearchStreamSite[];
}

/** `site_result` 事件：单站搜索成功，携带该站全部命中。 */
export interface SiteStreamResult {
  site_id: string;
  site_name: string;
  count: number;
  /** 该站从发起到返回的耗时（毫秒） */
  elapsed_ms: number;
  items: TorrentHit[];
}

/** `site_error` 事件：单站搜索失败（可读中文原因），不影响其它站点。 */
export interface SiteStreamError {
  site_id: string;
  site_name: string;
  error: string;
  elapsed_ms: number;
}

/** `done` 事件：所有站点均已返回的整体汇总。 */
export interface SearchStreamDone {
  total: number;
  elapsed_ms: number;
  sites: SiteSearchStatus[];
}

/** 流式搜索事件的可辨识联合，事件序列：start → site_start×N → (site_result|site_error)×N → done。 */
export type SearchStreamEvent =
  | { type: "start"; data: SearchStreamStart }
  | { type: "site_start"; data: SearchStreamSite }
  | { type: "site_result"; data: SiteStreamResult }
  | { type: "site_error"; data: SiteStreamError }
  | { type: "done"; data: SearchStreamDone };

/**
 * 流式跨站搜索：快的站点先出结果，逐事件回调 `onEvent`，全部结束后 resolve。
 *
 * 用 @microsoft/fetch-event-source 订阅 SSE：它能带 Authorization 头（全站
 * 鉴权一致），协议解析完整（多行 data / event / retry）。搜索是一次性动作，
 * 失败/断线应由用户显式重试——onerror 里 throw 即停止自动重连（库默认会
 * backoff 重连，对搜索流是重放副作用）。
 * 取消走 `init.signal`（AbortController），中止会以 AbortError reject。
 */
export async function streamSearchTorrents(
  params: SearchParams,
  onEvent: (event: SearchStreamEvent) => void,
  init?: RequestInit,
): Promise<void> {
  const url = resolveRequestUrl(`/search/torrents/stream?${searchParamsOf(params)}`);
  const token = getAuthToken();
  await fetchEventSource(url, {
    signal: init?.signal,
    headers: {
      Accept: "text/event-stream",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    openWhenHidden: true,
    async onopen(response) {
      if (response.ok) return;
      // 把非 2xx 收敛成 HttpError（401 走统一跳登录），并停止重连。
      let message = `Request failed with status ${response.status}`;
      let details: unknown = null;
      try {
        details = await response.json();
        if (details && typeof details === "object" && "message" in details) {
          message = String((details as { message: unknown }).message);
        }
      } catch {
        // 非 JSON 错误体，保留默认 message
      }
      redirectToLoginOn401(response.status);
      throw new HttpError(message, response.status, details);
    },
    onmessage(block) {
      if (!block.data) return;
      onEvent({ type: block.event, data: JSON.parse(block.data) } as SearchStreamEvent);
    },
    onerror(error) {
      if (error instanceof HttpError) throw error; // 认证/业务错误：终止
      if (init?.signal?.aborted) throw error; // 用户取消：终止
      throw error; // 网络断线：搜索是一次性动作，抛给调用方重试
    },
  });
}
