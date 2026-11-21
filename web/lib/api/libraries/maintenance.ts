import { request } from "@/lib/http";

import { type ApiEnvelope, num, refreshReceiptOf, unwrap } from "./helpers";
import type { DuplicateEntryGroup } from "./item-types";
import type {
  GenerateLibraryCoverOptions,
  MediaInfoRefreshMode,
  MediaInfoRefreshReceipt,
  MetadataRefreshReceipt,
  OrganizePreviewOptions,
  OrganizePreviewResult,
  PathMissingRow,
  ScanOperationResponse,
} from "./types";

/** 核验库内文件：缺失的 ledger 行打标、恢复的清除。返回 {missing, restored}。 */
export async function verifyLibraryFiles(
  libraryId: string,
): Promise<{ missing: number; restored: number }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${libraryId}/verify`, {
      method: "POST",
    }),
  );
  return { missing: num(data?.missing), restored: num(data?.restored) };
}

export async function listLibraryDuplicates(libraryId: string): Promise<DuplicateEntryGroup[]> {
  return unwrap(request<ApiEnvelope<DuplicateEntryGroup[]>>(`/libraries/${libraryId}/duplicates`));
}

export async function deleteDuplicateFile(
  libraryId: string,
  fileId: string,
): Promise<{ binned: boolean; path: string }> {
  return unwrap(
    request<ApiEnvelope<{ binned: boolean; path: string }>>(
      `/libraries/${libraryId}/duplicates/${fileId}`,
      { method: "DELETE" },
    ),
  );
}

/** 触发一次库扫描（POST /libraries/{id}/scan 返回 202 Accepted 与 operation_id）。 */
export async function startLibraryScan(id: string): Promise<ScanOperationResponse> {
  return unwrap(
    request<ApiEnvelope<ScanOperationResponse>>(`/libraries/${id}/scan`, { method: "POST" }),
  );
}

/** 预览整理计划（GET /libraries/{id}/organize-preview：按目标库/目标根与命名模板算目标名）。 */
export async function previewLibraryOrganize(
  id: string,
  options: OrganizePreviewOptions = {},
): Promise<OrganizePreviewResult> {
  const params = new URLSearchParams();
  if (options.targetLibraryId) params.set("target_library_id", options.targetLibraryId);
  if (options.targetRoot) params.set("target_root", options.targetRoot);
  const suffix = params.size > 0 ? `?${params}` : "";
  const init = options.signal ? { signal: options.signal } : undefined;
  return unwrap(request<ApiEnvelope<OrganizePreviewResult>>(`/libraries/${id}/organize-preview${suffix}`, init));
}

/** 开始整理（POST /libraries/{id}/organize：应用预览确认的重命名）。 */
export async function startLibraryOrganize(
  id: string,
  renames: { from: string; to: string; to_library_id?: string }[],
): Promise<{ applied: number; skipped: string[]; errors: string[] }> {
  return unwrap(
    request<ApiEnvelope<{ applied: number; skipped: string[]; errors: string[] }>>(
      `/libraries/${id}/organize`,
      { method: "POST", body: JSON.stringify({ renames }) },
    ),
  );
}

/** 整库刷新元数据（POST /libraries/{id}/metadata/refresh）。 */
export async function startLibraryMetadataRefresh(id: string): Promise<MetadataRefreshReceipt> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${id}/metadata/refresh`, {
      method: "POST",
    }),
  );
  return refreshReceiptOf(data ?? {});
}

/** 用普通 HTTP 页面也可用的随机源生成一次媒体信息抓取的 UUID。 */
export function createMediaInfoRequestId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** 分批为本库文件抓取媒体流信息；同一 requestId 可重试已提交的批次。 */
export async function refreshLibraryMediaInfo(
  id: string,
  mode: MediaInfoRefreshMode,
  requestId: string = createMediaInfoRequestId(),
): Promise<MediaInfoRefreshReceipt> {
  const receipt: MediaInfoRefreshReceipt = { mode, total: 0, queued: 0, skipped: 0 };
  let after: string | null = null;
  const cursors = new Set<string>();
  do {
    const data: MediaInfoRefreshReceipt & { next_cursor: string | null } = await unwrap(
      request<ApiEnvelope<MediaInfoRefreshReceipt & { next_cursor: string | null }>>(
        `/libraries/${id}/media-info/refresh`,
        { method: "POST", body: JSON.stringify({ mode, request_id: requestId, after }) },
      ),
    );
    if (
      !data ||
      data.mode !== mode ||
      ![data.total, data.queued, data.skipped].every(
        (value) => Number.isSafeInteger(value) && value >= 0,
      ) ||
      data.total !== data.queued + data.skipped ||
      (data.next_cursor !== null &&
        (typeof data.next_cursor !== "string" || !data.next_cursor || cursors.has(data.next_cursor)))
    ) {
      throw new Error("媒体信息抓取响应格式错误");
    }
    receipt.total += data.total;
    receipt.queued += data.queued;
    receipt.skipped += data.skipped;
    after = data.next_cursor;
    if (after) cursors.add(after);
  } while (after !== null);
  return receipt;
}

/** 上传或替换媒体库的自定义封面（横图）。 */
export async function uploadLibraryCover(
  libraryId: string,
  dataUrl: string,
): Promise<{ bytes: number; path: string }> {
  return unwrap(
    request<ApiEnvelope<{ bytes: number; path: string }>>(`/libraries/${libraryId}/cover`, {
      method: "POST",
      body: JSON.stringify({ data_url: dataUrl }),
    }),
  );
}

export async function generateLibraryCover(
  libraryId: string,
  options: GenerateLibraryCoverOptions,
): Promise<{ success?: boolean; cover_url?: string; data_url: string; preview?: boolean }> {
  // 生成 1080p 超清图片和编码耗时较多，设置充足的客户端超时时间 (180s)
  return unwrap(
    request<ApiEnvelope<{ success?: boolean; cover_url?: string; data_url: string; preview?: boolean }>>(
      `/libraries/${libraryId}/cover/generate`,
      { method: "POST", body: JSON.stringify(options) },
      { timeoutMs: 180_000 },
    ),
  );
}

/** 删除/清空媒体库的封面。 */
export async function deleteLibraryCover(libraryId: string): Promise<{ deleted: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/libraries/${libraryId}/cover`, {
      method: "DELETE",
    }),
  );
}

export async function previewPathReconciliation(
  libraryId: string,
): Promise<{ missing: PathMissingRow[] }> {
  return unwrap(
    request<ApiEnvelope<{ missing: PathMissingRow[] }>>(
      `/libraries/${libraryId}/path-reconciliation-preview`,
    ),
  );
}

export async function applyPathReconciliations(
  libraryId: string,
  reconciliations: { from: string; to: string }[],
): Promise<{ reconciled: number; errors: string[] }> {
  return unwrap(
    request<ApiEnvelope<{ reconciled: number; errors: string[] }>>(
      `/libraries/${libraryId}/path-reconciliations`,
      {
        method: "POST",
        body: JSON.stringify({ reconciliations }),
      },
    ),
  );
}
