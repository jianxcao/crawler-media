import type { FrameIdentity, FrameStatus } from "@/lib/chapter-frame-task";
import { request } from "@/lib/http";

import { type ApiEnvelope, unwrap } from "./helpers";

/** 后端章节形状 → ChapterStrip 的 LibraryChapter。 */
export interface ItemChapter {
  start_ms: number;
  end_ms: number;
  title: string | null;
  image_url: string | null;
}

export interface ChapterTarget {
  file_id: string;
  season?: number;
  episode?: number;
}

export interface RefreshedItemChapters {
  chapters: ItemChapter[];
  fingerprint_refresh_queued: boolean;
  fingerprint_refresh_already_running?: boolean;
  fingerprint_refresh_error?: string | null;
  fingerprint_refresh_job?: ProbeJobStatus | null;
}

export interface ProbeJobMetrics {
  input_bytes: number | null;
  measurement_complete?: boolean;
}

export interface ProbeJobStatus {
  id: string;
  kind: string;
  season: number | null;
  status: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  total: number;
  completed: number;
  succeeded: number;
  failed: number;
  error: string | null;
  elapsed_ms: number;
  phase?: string | null;
  sampling_mode?: string | null;
  queue_wait_ms?: number | null;
  priority_wait_ms?: number | null;
  metrics?: ProbeJobMetrics | null;
}

export interface ItemProbeStatus {
  active: boolean;
  job: ProbeJobStatus | null;
}

function chapterPath(
  libraryId: string,
  mediaItemId: string,
  action: string,
  target: ChapterTarget,
): string {
  const query = new URLSearchParams({ file_id: target.file_id });
  if (target.season !== undefined) query.set("season", String(target.season));
  if (target.episode !== undefined) query.set("episode", String(target.episode));
  return `/libraries/${libraryId}/items/${mediaItemId}/chapters${action}?${query}`;
}

export async function fetchItemChapters(
  libraryId: string,
  mediaItemId: string,
  target: ChapterTarget,
  signal?: AbortSignal,
): Promise<ItemChapter[]> {
  return unwrap(
    request<ApiEnvelope<ItemChapter[]>>(chapterPath(libraryId, mediaItemId, "", target), { signal }),
  );
}

export async function generateItemChapters(
  libraryId: string,
  mediaItemId: string,
  target: ChapterTarget,
  retryFailed = false,
  signal?: AbortSignal,
): Promise<FrameStatus> {
  return unwrap(
    request<ApiEnvelope<FrameStatus>>(chapterPath(libraryId, mediaItemId, "/generate", target), {
      method: "POST",
      body: JSON.stringify({ retry_failed: retryFailed }),
      signal,
    }),
  );
}

export async function fetchChapterFrameStatus(
  libraryId: string,
  mediaItemId: string,
  target: ChapterTarget,
  expected?: FrameIdentity,
  signal?: AbortSignal,
): Promise<FrameStatus> {
  let path = chapterPath(libraryId, mediaItemId, "/frame-status", target);
  if (expected) path += `&${new URLSearchParams(expected)}`;
  return unwrap(request<ApiEnvelope<FrameStatus>>(path, { signal }));
}

export async function refreshItemChapters(
  libraryId: string,
  mediaItemId: string,
  target: ChapterTarget,
  capturePolicy: "reuse_valid" | "recapture",
  signal?: AbortSignal,
): Promise<RefreshedItemChapters> {
  const data = await unwrap(
    request<ApiEnvelope<RefreshedItemChapters>>(
      chapterPath(libraryId, mediaItemId, "/refresh", target),
      { method: "POST", body: JSON.stringify({ capture_policy: capturePolicy }), signal },
    ),
  );
  if (!data || Array.isArray(data) || !Array.isArray(data.chapters)) {
    throw new Error(
      "chapter refresh contract violation: expected { chapters, fingerprint_refresh_queued }",
    );
  }
  return data;
}

export async function fetchItemProbeStatus(
  libraryId: string,
  mediaItemId: string,
  season: number,
  signal?: AbortSignal,
): Promise<ItemProbeStatus> {
  const path = `/libraries/${libraryId}/items/${mediaItemId}/probe-status?${new URLSearchParams({ season: String(season) })}`;
  return unwrap(request<ApiEnvelope<ItemProbeStatus>>(path, { signal }));
}

export interface SimilarHit {
  id: string | null;
  kind: string;
  title: string;
  year: number | null;
  tmdb_id: string | null;
  poster_url: string | null;
}

export type SimilarRead =
  | { state: "pending"; retry_after_ms: number }
  | { state: "ready"; items: SimilarHit[] };

/** Explicit pending means origin work is still running; ready always has full results. */
export async function fetchItemSimilar(
  libraryId: string,
  mediaItemId: string,
  signal?: AbortSignal,
): Promise<SimilarRead> {
  const result = await unwrap(
    request<ApiEnvelope<SimilarRead>>(`/libraries/${libraryId}/items/${mediaItemId}/similar`, {
      signal,
    }),
  );
  if (result?.state === "ready" && Array.isArray(result.items)) return result;
  if (result?.state === "pending" && Number.isFinite(result.retry_after_ms) && result.retry_after_ms > 0) {
    return result;
  }
  throw new Error("相似推荐响应格式错误");
}
