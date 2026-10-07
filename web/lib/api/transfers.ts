import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

export interface LedgerItem {
  id: string;
  media_id: string;
  library_id: string | null;
  media_title: string;
  media_kind: "movie" | "tv";
  path: string;
  source_path?: string | null;
  season: number | null;
  episode: number | null;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string;
  confidence: string;
  filter_score: number | null;
  transfer_mode: "hardlink" | "copy" | "move" | "strm";
  file_size: number;
}

export async function listLedger(): Promise<LedgerItem[]> {
  return unwrap(request<ApiEnvelope<LedgerItem[]>>("/ledger"));
}

export async function deleteLedgerRow(
  id: string,
  deleteFile = false,
): Promise<{ deleted: boolean; path: string; file_deleted: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean; path: string; file_deleted: boolean }>>(
      `/ledger/${id}?delete_file=${deleteFile}`,
      { method: "DELETE" },
    ),
  );
}

export async function retransferLedgerRow(
  id: string,
): Promise<{ retransferred: boolean; source: string; destination: string }> {
  return unwrap(
    request<ApiEnvelope<{ retransferred: boolean; source: string; destination: string }>>(
      `/ledger/${id}/retransfer`,
      { method: "POST" },
    ),
  );
}

export async function refreshItemMetadata(
  libraryId: string,
  itemId: string,
): Promise<{ ok: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ ok: boolean }>>(
      `/libraries/${libraryId}/items/${itemId}/metadata/refresh`,
      { method: "POST" },
    ),
  );
}
