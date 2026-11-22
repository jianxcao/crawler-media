import { request } from "@/lib/http";
import { collectAllFilePages } from "@/lib/library-file-pages";
import type { LibraryKind } from "@/lib/media-types";

import { type ApiEnvelope, bool, num, refreshReceiptOf, str, unwrap } from "./helpers";
import type {
  ArtworkCandidate,
  ArtworkCandidates,
  AudioStream,
  ItemDeleteResult,
  LibraryEpisode,
  LibraryItemBrand,
  LibraryItemDetail,
  LibraryItemFile,
  LibraryItemFilePage,
  LibrarySeasonSummary,
  SeasonEpisodes,
  SubtitleStream,
  VideoTrackInfo,
} from "./item-types";
import type { MetadataRefreshReceipt } from "./types";

/** 刷新单个条目的元数据（海报/背景/NFO；POST .../metadata/refresh）。 */
export async function refreshItemMetadata(
  libraryId: string,
  mediaItemId: string,
): Promise<MetadataRefreshReceipt> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/metadata/refresh`,
      { method: "POST" },
    ),
  );
  return refreshReceiptOf(data ?? {});
}

/** 条目详情（GET /libraries/{id}/items/{itemId}）。 */
type TrackFrom = Omit<AudioStream, "forced" | "external" | "file_name"> &
  Omit<SubtitleStream, "profile" | "channels" | "channel_layout">;

/** 后端音轨/字幕轨 JSON → 前端 AudioStream/SubtitleStream 形状。 */
function trackFrom(raw: unknown): TrackFrom {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    codec: r.codec == null ? null : str(r.codec),
    profile: r.profile == null ? null : str(r.profile),
    channels: r.channels == null ? null : num(r.channels),
    channel_layout: r.channel_layout == null ? null : str(r.channel_layout),
    language: r.language == null ? null : str(r.language),
    sample_rate: r.sample_rate == null ? null : str(r.sample_rate),
    title: r.title == null ? null : str(r.title),
    default: bool(r.default),
    forced: r.forced == null ? false : bool(r.forced),
    external: r.external == null ? false : bool(r.external),
    file_name: r.file_name == null ? null : str(r.file_name),
  };
}

function videoTrackFrom(raw: unknown): VideoTrackInfo {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    codec: r.codec == null ? null : str(r.codec),
    width: r.width == null ? null : num(r.width),
    height: r.height == null ? null : num(r.height),
    frame_rate: r.frame_rate == null ? null : num(r.frame_rate),
    bit_rate: r.bit_rate == null ? null : num(r.bit_rate),
    duration_secs: r.duration_secs == null ? null : num(r.duration_secs),
  };
}

function fileFrom(f: unknown): LibraryItemFile {
  const fr = (f ?? {}) as Record<string, unknown>;
  return {
    file_id: str(fr.file_id),
    path: str(fr.path),
    units: Array.isArray(fr.units) ? (fr.units as unknown[]).map(unitFrom) : [],
    // 缺字段不再 fail-open：未知在位/发布状态一律视为不可播。
    presence: presenceFrom(fr.presence),
    publication: fr.publication === "published" ? "published" : "pending",
    season: fr.season == null ? null : num(fr.season),
    episode: fr.episode == null ? null : num(fr.episode),
    resolution: fr.resolution == null ? null : str(fr.resolution),
    codec: fr.codec == null ? null : str(fr.codec),
    hdr: fr.hdr == null ? null : str(fr.hdr),
    quality_source: fr.quality_source == null ? null : str(fr.quality_source),
    audio_tracks: Array.isArray(fr.audio_tracks) ? (fr.audio_tracks as unknown[]).map(trackFrom) : [],
    subtitle_tracks: Array.isArray(fr.subtitle_tracks)
      ? (fr.subtitle_tracks as unknown[]).map(trackFrom)
      : [],
    video_track: fr.video_track == null ? null : videoTrackFrom(fr.video_track),
    probed: fr.probed === true,
    probe_queued: fr.probe_queued === true,
  };
}

function unitFrom(u: unknown): { season: number; episode: number } {
  const ur = (u ?? {}) as Record<string, unknown>;
  return { season: num(ur.season), episode: num(ur.episode) };
}

function presenceFrom(value: unknown): "present" | "missing" | "unknown" {
  return value === "present" || value === "missing" ? value : "unknown";
}

function brandFrom(raw: unknown): LibraryItemBrand | null {
  const r = (raw ?? {}) as Record<string, unknown>;
  const name = str(r.name).trim();
  if (!name) return null;
  const logoRaw = r.logo_url == null ? null : str(r.logo_url).trim();
  const kind = r.kind === "network" || r.kind === "studio" ? r.kind : undefined;
  return {
    name,
    logo_url: logoRaw ? logoRaw : null,
    ...(kind ? { kind } : {}),
  };
}

function seasonSummaryFrom(raw: unknown): LibrarySeasonSummary {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    season_number: num(r.season_number),
    owned_count: num(r.owned_count),
    total_count: num(r.total_count),
    downloading_count: num(r.downloading_count),
  };
}

export async function getLibraryItemDetail(
  libraryId: string,
  mediaItemId: string,
  init?: RequestInit,
): Promise<LibraryItemDetail> {
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(`/libraries/${libraryId}/items/${mediaItemId}`, init),
  );
  const r = (raw ?? {}) as Record<string, unknown>;
  const firstPage = {
    files: Array.isArray(r.files) ? r.files.map(fileFrom) : [],
    next_cursor: r.next_file_cursor == null ? null : str(r.next_file_cursor),
  };
  const files = await getAllLibraryItemFiles(libraryId, mediaItemId, firstPage, init);
  return {
    media_item_id: str(r.media_item_id),
    kind: r.kind as LibraryKind,
    title: str(r.title),
    year: r.year == null ? null : num(r.year),
    original_title: r.original_title == null ? null : str(r.original_title),
    tmdb_id: r.tmdb_id == null ? null : str(r.tmdb_id),
    overview: r.overview == null ? null : str(r.overview),
    rating: r.rating == null ? null : str(r.rating),
    content_rating: r.content_rating == null ? null : str(r.content_rating),
    runtime_minutes: r.runtime_minutes == null ? null : str(r.runtime_minutes),
    genres: Array.isArray(r.genres) ? (r.genres as unknown[]).map((g) => str(g)) : [],
    brands: Array.isArray(r.brands)
      ? (r.brands as unknown[])
          .map(brandFrom)
          .filter((brand): brand is LibraryItemBrand => brand !== null)
      : [],
    cast: Array.isArray(r.cast)
      ? (r.cast as unknown[]).map((c) => {
          const cr = (c ?? {}) as Record<string, unknown>;
          return {
            name: str(cr.name),
            role: cr.role == null ? null : str(cr.role),
            tmdb_person_id: cr.tmdb_person_id == null ? null : num(cr.tmdb_person_id),
            avatar_url: cr.avatar_url == null ? null : str(cr.avatar_url),
          };
        })
      : [],
    poster_url: r.poster_url == null ? null : str(r.poster_url),
    backdrop_url: r.backdrop_url == null ? null : str(r.backdrop_url),
    seasons: Array.isArray(r.seasons) ? r.seasons.map(num) : [],
    season_summaries: Array.isArray(r.season_summaries)
      ? (r.season_summaries as unknown[]).map(seasonSummaryFrom)
      : [],
    files,
  };
}

/** One bounded page of a detail item's files (`GET .../files?after=`). */
export async function getLibraryItemFiles(
  libraryId: string,
  mediaItemId: string,
  after: string,
  init?: RequestInit,
): Promise<LibraryItemFilePage> {
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(
      `/libraries/${libraryId}/items/${mediaItemId}/files?after=${encodeURIComponent(after)}`,
      init,
    ),
  );
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    files: Array.isArray(r.files) ? (r.files as unknown[]).map(fileFrom) : [],
    // 续页 `GET .../files?after=` 的游标字段是 `next_cursor`；首详情页才叫 `next_file_cursor`。
    next_cursor: r.next_cursor == null ? null : str(r.next_cursor),
  };
}

export async function getAllLibraryItemFiles(
  libraryId: string,
  mediaItemId: string,
  firstPage: LibraryItemFilePage,
  init?: RequestInit,
): Promise<LibraryItemFile[]> {
  // 分页遍历只有一处实现：`lib/library-file-pages.ts` 负责不重不漏与末页判定，
  // 这里只注入「按 cursor 取下一页」。
  return collectAllFilePages(firstPage, async (after) => {
    const page = await getLibraryItemFiles(libraryId, mediaItemId, after, init);
    if (page.files.length === 0) throw new Error("文件分页 cursor 未推进");
    return page;
  });
}

/** 剧集条目一季的分集清单（GET /libraries/{id}/items/{itemId}/episodes?season=）。 */
export async function getItemEpisodes(
  libraryId: string,
  mediaItemId: string,
  seasonNumber: number,
): Promise<SeasonEpisodes> {
  const episodes = await unwrap(
    request<ApiEnvelope<LibraryEpisode[]>>(
      `/libraries/${libraryId}/items/${mediaItemId}/episodes?season=${seasonNumber}`,
    ),
  );
  episodes.sort((a, b) => a.episode_number - b.episode_number);
  return { season_number: seasonNumber, episodes };
}

/**
 * 手动触发完整探测：清空条目所有文件的流信息/章节/片头片尾缓存并重新入队
 * 后台探测（streamdetails + 声纹指纹），前端随后轮询刷新可见。
 */
export async function probeItem(
  libraryId: string,
  mediaItemId: string,
  signal?: AbortSignal,
): Promise<{ queued: number; already_running: boolean }> {
  const envelope = await request<ApiEnvelope<{ queued: number; already_running: boolean }>>(
    `/libraries/${libraryId}/items/${mediaItemId}/probe`,
    { method: "POST", signal },
  );
  return envelope.data;
}

/** 条目的候选海报/背景（GET /libraries/{id}/items/{itemId}/artwork/candidates）。 */
export async function listArtworkCandidates(
  libraryId: string,
  mediaItemId: string,
): Promise<ArtworkCandidates> {
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/candidates`,
    ),
  );
  const r = (raw ?? {}) as Record<string, unknown>;
  const list = (key: string): ArtworkCandidate[] =>
    Array.isArray(r[key])
      ? (r[key] as unknown[]).map((c) => {
          const cr = (c ?? {}) as Record<string, unknown>;
          return {
            file_path: str(cr.file_path),
            preview_url: str(cr.preview_url),
            width: cr.width == null ? null : num(cr.width),
            height: cr.height == null ? null : num(cr.height),
            language: cr.language == null ? null : str(cr.language),
            vote_average: cr.vote_average == null ? null : num(cr.vote_average),
            vote_count: cr.vote_count == null ? null : num(cr.vote_count),
          };
        })
      : [];
  return {
    posters: list("posters"),
    backdrops: list("backdrops"),
    current_poster: r.current_poster == null ? null : str(r.current_poster),
    current_backdrop: r.current_backdrop == null ? null : str(r.current_backdrop),
    poster_locked: bool(r.poster_locked),
    backdrop_locked: bool(r.backdrop_locked),
  };
}

/**
 * 选定海报/背景（POST /libraries/{id}/items/{itemId}/artwork/select）。
 * `filePath` 传 null = 解锁并恢复自动选图。
 */
export async function selectArtwork(
  libraryId: string,
  mediaItemId: string,
  kind: "poster" | "backdrop",
  filePath: string | null,
): Promise<{ locked: boolean }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/select`,
      { method: "POST", body: JSON.stringify({ kind, file_path: filePath }) },
    ),
  );
  return { locked: bool((data ?? {}).locked ?? true) };
}

/** 上传本地自定义海报（JPEG/PNG/WebP，最大 12MB）。 */
export async function uploadArtwork(
  libraryId: string,
  mediaItemId: string,
  dataUrl: string,
): Promise<{ locked: boolean; bytes: number }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/upload`,
      { method: "POST", body: JSON.stringify({ data_url: dataUrl }) },
    ),
  );
  return {
    locked: bool((data ?? {}).locked ?? true),
    bytes: num((data ?? {}).bytes ?? 0),
  };
}

/** 从磁盘彻底删除条目：台账行 + 库根下的文件。 */
export function deleteLibraryItem(
  libraryId: string,
  mediaItemId: string,
): Promise<ItemDeleteResult> {
  return unwrap(
    request<ApiEnvelope<ItemDeleteResult>>(`/libraries/${libraryId}/items/${mediaItemId}`, {
      method: "DELETE",
    }),
  );
}
