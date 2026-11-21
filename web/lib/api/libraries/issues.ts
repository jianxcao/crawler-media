import { request } from "@/lib/http";
import type { LibraryKind } from "@/lib/media-types";

import { type ApiEnvelope, num, str, unwrap } from "./helpers";
import type {
  MissingItem,
  ReidentifyPreview,
  ReidentifyRequest,
  UnidentifiedCandidate,
  UnidentifiedCode,
  UnidentifiedFile,
  UnidentifiedGroup,
} from "./item-types";

/** 修正识别预览（GET .../reidentification-preview）。 */
export async function previewItemReidentification(
  libraryId: string,
  mediaItemId: string,
): Promise<ReidentifyPreview> {
  return unwrap(
    request<ApiEnvelope<ReidentifyPreview>>(
      `/libraries/${libraryId}/items/${mediaItemId}/reidentification-preview`,
    ),
  );
}

/** 待识别清单（GET /unidentified），按条目目录分组展示。 */
export async function listUnidentifiedLibraryFiles(
  libraryId?: string,
): Promise<UnidentifiedGroup[]> {
  const qs = libraryId ? `?library_id=${libraryId}` : "";
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/unidentified${qs}`));
  return toUnidentifiedGroups(raw);
}

function toUnidentifiedGroups(raw: unknown): UnidentifiedGroup[] {
  if (!Array.isArray(raw)) throw new Error("待识别清单响应格式错误");
  return raw.map((entry) => {
    const r = (entry ?? {}) as Record<string, unknown>;
    const libraryId = str(r.library_id);
    const libraryName = str(r.library_name);
    return {
      key: str(r.key ?? ""),
      label: str(r.label ?? "未识别文件"),
      library_id: libraryId,
      library_name: libraryName,
      file_count: num(r.file_count),
      total_size_bytes: num(r.total_size_bytes),
      reason: r.reason == null ? null : str(r.reason),
      code: (r.code as UnidentifiedCode) ?? null,
      candidates: Array.isArray(r.candidates) ? (r.candidates as UnidentifiedCandidate[]) : [],
      files: Array.isArray(r.files)
        ? (r.files as unknown[]).map((f) => toUnidentifiedFile(f, libraryId, libraryName))
        : [],
    };
  });
}

/** 后端每个文件只给 `file_id` / `file_path`；其余字段来自分组或为空。 */
function toUnidentifiedFile(
  raw: unknown,
  libraryId: string,
  libraryName: string,
): UnidentifiedFile {
  const f = (raw ?? {}) as Record<string, unknown>;
  return {
    id: str(f.file_id ?? ""),
    library_id: libraryId,
    library_name: libraryName,
    file_path: str(f.file_path ?? ""),
    size_bytes: num(f.size_bytes),
    season_number: num(f.season_number),
    episode_number: num(f.episode_number),
    reason: f.reason == null ? null : str(f.reason),
    code: (f.code as UnidentifiedCode) ?? null,
    candidates: Array.isArray(f.candidates) ? (f.candidates as UnidentifiedCandidate[]) : [],
  };
}

/** 把条目身份改挂到目标作品（POST .../reidentifications）。 */
export async function assignLibraryFilesToTitle(
  body: ReidentifyRequest,
): Promise<{ claimed: number }> {
  return unwrap(
    request<ApiEnvelope<{ claimed: number }>>("/reidentify", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  );
}

/** 认领一个未识别文件为新识别作品（POST /unidentified/claim）。 */
export async function claimUnidentifiedFile(body: {
  path: string;
  title: string;
  kind: "movie" | "tv";
  year: number | null;
  tmdb_id: string;
}): Promise<unknown> {
  return unwrap(
    request<ApiEnvelope<unknown>>("/unidentified/claim", {
      method: "POST",
      body: JSON.stringify({
        path: body.path,
        action: {
          type: "identify_and_transfer",
          title: body.title,
          kind: body.kind,
          year: body.year,
          tmdb_id: body.tmdb_id,
        },
      }),
    }),
  );
}

/** 标为「非独立作品」（POST .../reidentifications/extras）。 */
export async function markLibraryFilesAsExtras(fileIds: string[]): Promise<{ detached: number }> {
  return unwrap(
    request<ApiEnvelope<{ detached: number }>>("/reidentify/extras", {
      method: "POST",
      body: JSON.stringify({ file_ids: fileIds }),
    }),
  );
}

/** 缺失清单（GET /libraries/{id}/missing），按条目聚合。 */
export async function listMissingLibraryFiles(libraryId: string): Promise<MissingItem[]> {
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/libraries/${libraryId}/missing`));
  if (!Array.isArray(raw)) throw new Error("缺失清单响应格式错误");
  return raw.map((entry) => {
    const r = (entry ?? {}) as Record<string, unknown>;
    return {
      media_item_id: str(r.media_item_id),
      kind: (r.kind as LibraryKind) ?? "movie",
      tmdb_id: r.tmdb_id == null ? null : str(r.tmdb_id),
      title: str(r.title),
      year: r.year == null ? null : num(r.year),
      poster_url: r.poster_url == null ? null : str(r.poster_url),
      subscription_id: r.subscription_id == null ? null : str(r.subscription_id),
      files: Array.isArray(r.files)
        ? (r.files as unknown[]).map((f) => {
            const fr = (f ?? {}) as Record<string, unknown>;
            return {
              id: str(fr.id ?? ""),
              file_path: str(fr.file_path ?? ""),
              season_number: num(fr.season_number),
              episode_number: num(fr.episode_number),
              size_bytes: num(fr.size_bytes),
            };
          })
        : [],
    };
  });
}

/** 删除「文件缺失」的 ledger 行（用户确认文件已删后清记录）。可传 mediaItemId 仅清除单部作品。 */
export async function deleteMissingRows(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ deleted: number }> {
  const qs = mediaItemId ? `?media_item_id=${encodeURIComponent(mediaItemId)}` : "";
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${libraryId}/missing-rows${qs}`, {
      method: "DELETE",
    }),
  );
  return { deleted: num(data?.deleted) };
}

/** 清理缺失记录（DELETE /libraries/{id}/missing-rows）。带 mediaItemId 时精确针对单条目。 */
export async function clearMissingLibraryRecords(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ cleared: number }> {
  const result = await deleteMissingRows(libraryId, mediaItemId);
  return { cleared: result.deleted };
}
