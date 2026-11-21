import { publicEnv } from "@/lib/env";
import { getPlayerDeviceId } from "@/lib/player/device";
import { getAuthToken, request, resolveRequestUrl } from "@/lib/http";
import type { LibraryEpisode } from "@/lib/api/libraries";
import type {
  ApiEnvelope,
  DecideBody,
  FavoritePageParams,
  FavoritesGalleryPage,
  FavoritesPage,
  FavoriteSortParams,
  MediaActivityScope,
  MediaActivitySnapshot,
  PlaybackApiScope,
  PlaybackChapterMark,
  PlaybackHistoryClearResult,
  PlaybackItemInfo,
  PlaybackLogEntry,
  PlaybackMarks,
  PlaybackMarkTarget,
  PlaybackMetricPayload,
  PlaybackPolicy,
  PlaybackProgressBody,
  PlaybackSession,
  PlaybackUnit,
  PlaybackWatchState,
  PlaybackWatchStats,
  ResumeParams,
  UpNextItem,
} from "./playback-types";

export type * from "./playback-types";

export const DEFAULT_PLAYBACK_SCOPE: PlaybackApiScope = {
  base: "/playback",
  progress: "server",
  telemetry: true,
};

/**
 * 取流/字幕地址的最终解析。
 *
 * 后端给的是**服务器根相对路径**（`/api/v1/playback/...`，token 已在里面），
 * 与 `request()` 吃的「API 相对路径」不是一回事，不能过 `resolveRequestUrl`
 * ——那会把前缀拼两遍。默认部署（同源 + Next 反代）直接可用；只有把
 * 同源地址不需要额外补上源。
 */
export function resolveStreamUrl(path: string): string {
  if (/^https?:\/\//.test(path)) return path;
  const base = publicEnv.apiBaseUrl;
  if (!/^https?:\/\//.test(base)) return path;
  return new URL(path, base).toString();
}

/** 当前账号"接下来该接着看"的作品；看完的不在其中。 */
export async function listUpNext(limit = 20): Promise<UpNextItem[]> {
  const response = await request<ApiEnvelope<{ items: UpNextItem[] }>>(
    `/playback/up-next?limit=${limit}`,
  );
  return response.data.items;
}

export const FAVORITE_SORTS = ["favorited_at", "title", "year"] as const;

function favoriteQuery(params: FavoriteSortParams & { unwatched_first?: boolean }): URLSearchParams {
  const query = new URLSearchParams();
  if (params.sort) query.set("sort", params.sort);
  if (params.order) query.set("order", params.order);
  if (params.unwatched_first) query.set("unwatched_first", "true");
  return query;
}

export async function listFavorites(params: FavoritePageParams = {}): Promise<FavoritesPage> {
  const query = favoriteQuery(params);
  query.set("limit", String(params.limit ?? 20));
  if (params.after) query.set("after", params.after);
  const response = await request<ApiEnvelope<FavoritesPage>>(`/playback/favorites?${query}`);
  return response.data;
}

export async function listFavoritesGallery(params: FavoritePageParams = {}): Promise<FavoritesGalleryPage> {
  const query = favoriteQuery(params);
  query.set("limit", String(params.limit ?? 20));
  if (params.after) query.set("after", params.after);
  const response = await request<ApiEnvelope<FavoritesGalleryPage>>(`/playback/favorites/gallery?${query}`);
  return response.data;
}

export async function seekFavoritesPosition(
  params: FavoriteSortParams & { start: number; unwatched_first?: boolean },
): Promise<{ start: number; after: string | null }> {
  const query = favoriteQuery(params);
  query.set("start", String(params.start));
  const response = await request<ApiEnvelope<{ start: number; after: string | null }>>(
    `/playback/favorites/position?${query}`,
  );
  return response.data;
}

/**
 * 清除**自己的**观看记录（续播点、已看标记、播放次数与播放质量指标）。
 * scope=item 需 mediaItemId，scope=library 需 libraryId，scope=all 清全部。
 * `since` 叠加时间窗口：只清这个时刻之后播放过的记录（首页「清空今天 /
 * 最近一周」用它，起点按浏览器本地时间算好再传）。
 */
export async function clearPlaybackHistory(
  scope: "item" | "library" | "all",
  target: { mediaItemId?: string; libraryId?: string; since?: Date } = {},
): Promise<PlaybackHistoryClearResult> {
  const params = new URLSearchParams({ scope });
  if (target.mediaItemId != null) params.set("media_item_id", String(target.mediaItemId));
  if (target.libraryId != null) params.set("library_id", String(target.libraryId));
  if (target.since != null) params.set("since", String(Math.floor(target.since.getTime() / 1000)));
  const response = await request<ApiEnvelope<PlaybackHistoryClearResult>>(
    `/playback/history?${params}`,
    { method: "DELETE" },
  );
  return response.data;
}

/** 活动页「观看」视角的实时快照：正在播放与正在下载。 */
export async function fetchMediaActivity(
  scope: MediaActivityScope = "visible",
): Promise<MediaActivitySnapshot> {
  const response = await request<ApiEnvelope<MediaActivitySnapshot>>(
    `/playback/activity?scope=${scope}`,
  );
  return response.data;
}

/** 结束一台设备本次播放：不动凭据，设备下次亲手点播放即可继续。 */
export async function endDevicePlayback(userId: string, deviceId: string): Promise<string> {
  const response = await request<ApiEnvelope<{ ended: number }>>(
    `/playback/activity/sessions/${encodeURIComponent(deviceId)}/end?user_id=${encodeURIComponent(userId)}`,
    { method: "POST" },
  );
  if (response.data?.ended !== 1) throw new Error("结束播放没有确认");
  return "已结束该设备的播放";
}

export interface PlaybackHistory {
  entries: PlaybackLogEntry[];
  hidden_count: number;
  /** 游标翻页：本页之后还有没有；有则两个 next_cursor 字段须成对回传。 */
  has_more: boolean;
  next_cursor: number | null;
  next_cursor_id: string | null;
}

export async function fetchPlaybackHistory(
  options: {
    limit?: number;
    /** 复合游标的时间部分：上一页的 next_cursor。 */
    before?: number | null;
    /** 复合游标的稳定排序部分：上一页的 next_cursor_id。 */
    beforeId?: string | null;
    days?: number;
    /** 按用户 UUID 筛选 */
    memberId?: string | null;
    scope?: MediaActivityScope;
  } = {},
): Promise<PlaybackHistory> {
  const params = new URLSearchParams({ scope: options.scope ?? "visible" });
  if (options.limit != null) params.set("limit", String(options.limit));
  if (options.before != null) params.set("before", String(options.before));
  if (options.beforeId != null) params.set("before_id", options.beforeId);
  if (options.days != null) params.set("days", String(options.days));
  if (options.memberId != null) params.set("member_id", String(options.memberId));
  const response = await request<ApiEnvelope<PlaybackHistory>>(`/playback/history?${params}`);
  return response.data;
}

/** 最近 N 天的观看统计；按天分组用浏览器时区，晚上的观看不会被算到第二天。 */
export async function fetchPlaybackWatchStats(
  days: number,
  scope: MediaActivityScope = "visible",
  memberId: string | null = null,
): Promise<PlaybackWatchStats> {
  const params = new URLSearchParams({
    days: String(days),
    scope,
    tz_offset: String(-new Date().getTimezoneOffset()),
  });
  if (memberId != null) params.set("member_id", String(memberId));
  const response = await request<ApiEnvelope<PlaybackWatchStats>>(
    `/playback/stats/watch?${params}`,
  );
  return response.data;
}

/** 注销一台播放器设备：凭据即刻失效，其正在进行的播放与取流一并停止。 */
export async function revokePlaybackDevice(userId: string, deviceId: string): Promise<string> {
  const response = await request<ApiEnvelope<{ revoked: boolean }>>(
    `/playback/devices/${encodeURIComponent(deviceId)}?user_id=${encodeURIComponent(userId)}`,
    { method: "DELETE" },
  );
  if (response.data?.revoked !== true) throw new Error("注销设备没有确认");
  return "已注销该设备";
}

/**
 * 判定档位并（需要时）起转码会话，返回可直接播放的地址。
 *
 * 带上浏览器设备标识：服务端把它写进取流 token，取流字节才能记到活动页上
 * 这台浏览器的会话名下（与进度上报同一个标识）。
 */
export async function startPlaybackSession(
  body: DecideBody,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<PlaybackSession> {
  const response = await request<ApiEnvelope<PlaybackSession>>(`${scope.base}/sessions`, {
    method: "POST",
    body: JSON.stringify({ ...body, device_id: getPlayerDeviceId() }),
  });
  return response.data;
}

/** 仅读取章节缓存，不触发新的媒体探测。 */
export async function fetchPlaybackChapters(
  playSessionId: string,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<{ ready: boolean; chapters: PlaybackChapterMark[] }> {
  const query = new URLSearchParams({ play_session_id: playSessionId, device_id: getPlayerDeviceId() });
  const response = await request<ApiEnvelope<{ ready: boolean; chapters: PlaybackChapterMark[] }>>(
    `${scope.base}/chapters?${query}`,
  );
  return response.data;
}

/**
 * 播放器客户端事件上报：把浏览器侧现场（MediaError 详情、播放器状态）落
 * 服务端日志。iPhone 上没有可看的控制台，这是拿到客户端真相的唯一通道。
 * fire-and-forget：上报失败绝不能影响播放本身。
 */
export function reportPlaybackClientLog(
  event: string,
  detail: Record<string, unknown>,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): void {
  if (!scope.telemetry) return;
  void request<ApiEnvelope<unknown>>(`${scope.base}/client-log`, {
    method: "POST",
    body: JSON.stringify({ event, detail }),
  }).catch(() => undefined);
}

export async function getPlaybackItem(
  mediaItemId: string,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<PlaybackItemInfo> {
  const response = await request<ApiEnvelope<PlaybackItemInfo>>(
    `${scope.base}/items/${mediaItemId}`,
  );
  return response.data;
}

/** 播放页一季的分集清单（切集/上一集下一集数据源），形状与库详情页同构。 */
export async function getPlaybackItemEpisodes(
  mediaItemId: string,
  seasonNumber: number,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<{ season_number: number; episodes: LibraryEpisode[] }> {
  const response = await request<
    ApiEnvelope<{ season_number: number; episodes: LibraryEpisode[] }>
  >(`${scope.base}/items/${mediaItemId}/episodes?season_number=${seasonNumber}`);
  return response.data;
}

/**
 * 续播查询串。
 *
 * 季集是**一对**坐标：只有季号或只有集号都不是合法单元，缺一半时两个都不发
 * （旧实现会把缺的那半序列化成字符串 "undefined" 发给服务端）。
 */
export function buildResumeQuery(params: ResumeParams): URLSearchParams {
  const query = new URLSearchParams({ media_item_id: String(params.media_item_id) });
  if (params.season_number != null && params.episode_number != null) {
    query.set("season_number", String(params.season_number));
    query.set("episode_number", String(params.episode_number));
  }
  // 同一份可选显式源偏好，由服务端 canonical resolver 解释 (R10)
  if (params.file_id) query.set("file_id", String(params.file_id));
  return query;
}

/** 起播前问「上次看到哪、用的哪条轨」。从未播过返回全零，不是错误。 */
export async function fetchResumeState(
  unit: PlaybackUnit,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<PlaybackWatchState> {
  const response = await request<ApiEnvelope<PlaybackWatchState>>(
    `${scope.base}/resume?${buildResumeQuery(unit)}`,
  );
  return response.data;
}

function markParams(target: PlaybackMarkTarget): URLSearchParams {
  const params = new URLSearchParams({ media_item_id: String(target.media_item_id) });
  if (target.season_number != null) params.set("season_number", String(target.season_number));
  if (target.episode_number != null) params.set("episode_number", String(target.episode_number));
  return params;
}

/** 目标的已看 / 收藏状态（详情页心与对勾的初始态）。 */
export async function fetchPlaybackMarks(target: PlaybackMarkTarget): Promise<PlaybackMarks> {
  const response = await request<ApiEnvelope<PlaybackMarks>>(
    `/playback/marks?${markParams(target)}`,
  );
  return response.data;
}

/**
 * 标记已看 / 收藏。与 Jellyfin 的 UserPlayedItems / UserFavoriteItems 走同一个
 * 服务：Infuse 里看到的与这里点的完全一致。返回写完后的状态，直接刷新按钮。
 */
export async function setPlaybackMarks(
  target: PlaybackMarkTarget,
  change: { played?: boolean; favorite?: boolean },
): Promise<PlaybackMarks> {
  const response = await request<ApiEnvelope<PlaybackMarks>>("/playback/marks", {
    method: "POST",
    body: JSON.stringify({
      media_item_id: target.media_item_id,
      season_number: target.season_number,
      episode_number: target.episode_number,
      ...change,
    }),
  });
  return response.data;
}

/** 进度上报体统一带上浏览器设备标识（活动页「正在播放」按它区分会话）。 */
function withDevice(body: PlaybackProgressBody): PlaybackProgressBody & { device_id: string } {
  return { ...body, device_id: getPlayerDeviceId() };
}

/** 上报观看进度（开始 / 心跳 / 停止同一入口）。 */
export async function reportPlaybackProgress(
  body: PlaybackProgressBody,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<PlaybackWatchState> {
  const response = await request<ApiEnvelope<PlaybackWatchState>>(`${scope.base}/progress`, {
    method: "POST",
    body: JSON.stringify(withDevice(body)),
  });
  return response.data;
}

/**
 * 页面卸载时上报最后位置。
 *
 * 这一条**不能丢**——丢了用户下次续播就会回到十几秒前，
 * 是最容易被察觉的体验缺陷。`sendBeacon` 是唯一能在卸载路径上活下来的
 * 通道（普通 fetch 会被直接取消），它发的是 POST，正合进度接口。
 * Blob 必须带 `application/json` 类型，否则 FastAPI 会因 Content-Type
 * 不对而拒收。
 */
export function reportPlaybackProgressOnUnload(
  body: PlaybackProgressBody,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): void {
  const payload = JSON.stringify(withDevice(body));
  const url = resolveRequestUrl(`${scope.base}/progress`);
  const token = getAuthToken();
  // 跨域且只靠 Bearer 时 sendBeacon 带不上 Authorization，最终进度会被 401。
  // keepalive fetch 能带上 token，并且在页面卸载后继续发出。
  if (typeof fetch !== "undefined" && token) {
    void fetch(url, {
      signal: AbortSignal.timeout(15_000),
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: payload,
      keepalive: true,
      credentials: "same-origin",
    }).catch(() => undefined);
    return;
  }
  if (typeof navigator === "undefined" || !navigator.sendBeacon) return;
  navigator.sendBeacon(url, new Blob([payload], { type: "application/json" }));
}

/** 按字段增量保存：没给的项保持原值，不会覆盖别处刚改的设置。 */
export async function savePlaybackPolicy(
  changes: Partial<Omit<PlaybackPolicy, "hardware_available" | "hw_backends">>,
): Promise<PlaybackPolicy> {
  const response = await request<ApiEnvelope<PlaybackPolicy>>("/playback/policy", {
    method: "PUT",
    body: JSON.stringify(changes),
  });
  return response.data;
}

/**
 * 上报一次播放的质量快照。
 *
 * 只落本地——写进自建实例自己的数据库，绝不外发。失败无所谓（指标是趋势
 * 数据），所以调用方一律吞掉；页面卸载路径用 sendBeacon 保证发得出去。
 */
export async function reportPlaybackMetric(
  payload: PlaybackMetricPayload,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): Promise<void> {
  if (!scope.telemetry) return;
  await request<ApiEnvelope<unknown>>("/playback/metrics", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

/** 卸载路径上的上报：普通 fetch 会被浏览器直接取消。 */
export function reportPlaybackMetricOnUnload(
  payload: PlaybackMetricPayload,
  scope: PlaybackApiScope = DEFAULT_PLAYBACK_SCOPE,
): void {
  if (!scope.telemetry) return;
  try {
    navigator.sendBeacon?.(
      resolveRequestUrl("/playback/metrics"),
      new Blob([JSON.stringify(payload)], { type: "application/json" }),
    );
  } catch {
    // 卸载路径上无处呈现错误
  }
}
