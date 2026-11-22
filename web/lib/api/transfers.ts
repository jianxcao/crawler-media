import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

export type TransferMode = "hardlink" | "copy" | "move" | "strm";
export type TransferViewMode = "files" | "groups";
export type TransferFilterMode = "all" | TransferMode;
export type TransferKindFilter = "all" | "movie" | "tv";

export interface LedgerItem {
  id: string;
  media_id: string;
  library_id: string | null;
  media_title: string;
  media_kind: "movie" | "tv";
  path: string;
  source_path?: string | null;
  units: Array<{ season: number; episode: number }>;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string;
  confidence: string;
  filter_score: number | null;
  transfer_mode: TransferMode;
  file_size: number;
}

export interface TransferSummary {
  total_files: number;
  media_count: number;
  movie_files: number;
  tv_files: number;
  strm_count: number;
  hardlink_count: number;
  copy_count: number;
  move_count: number;
  local_size_bytes: number;
  probed_duration_ms: number;
  strm_probed_count: number;
}

export interface TransferFileItem {
  id: string;
  media_id: string;
  library_id: string | null;
  media_title: string;
  media_kind: "movie" | "tv" | "video";
  path: string;
  source_path?: string | null;
  units: Array<{ season: number; episode: number }>;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string;
  confidence: string;
  filter_score: number | null;
  is_strm: boolean;
  transfer_mode: TransferMode;
  file_size: number | null;
  duration_ms: number | null;
}

export interface TransferGroupItem {
  media_id: string;
  media_title: string;
  media_kind: "movie" | "tv" | "video";
  library_id: string | null;
  file_count: number;
  strm_count: number;
  hardlink_count: number;
  copy_count?: number;
  move_count?: number;
  local_size_bytes: number;
  total_duration_ms: number;
  primary_resolution: string | null;
  primary_mode: TransferMode;
  sample_dir: string;
}

export interface TransferQueryParams {
  view?: TransferViewMode;
  q?: string;
  mode?: TransferFilterMode;
  kind?: TransferKindFilter;
  media_id?: string;
  offset?: number;
  limit?: number;
}

export interface TransferPageResponse {
  summary: TransferSummary;
  view: TransferViewMode;
  filtered_total: number;
  filtered_file_total: number;
  filtered_group_total?: number;
  offset: number;
  limit: number;
  has_more: boolean;
  items: TransferFileItem[];
  groups: TransferGroupItem[];
}

export async function listTransfers(
  params: TransferQueryParams = {},
  init?: RequestInit,
): Promise<TransferPageResponse> {
  const search = new URLSearchParams();
  if (params.view) search.set("view", params.view);
  const trimmedQ = params.q?.trim();
  if (trimmedQ) search.set("q", trimmedQ);
  if (params.mode && params.mode !== "all") search.set("mode", params.mode);
  else if (params.mode === "all") search.set("mode", "all");
  if (params.kind && params.kind !== "all") search.set("kind", params.kind);
  else if (params.kind === "all") search.set("kind", "all");
  if (params.media_id) search.set("media_id", params.media_id);
  if (params.offset != null) search.set("offset", String(params.offset));
  if (params.limit != null) search.set("limit", String(params.limit));

  const qs = search.toString();
  const path = qs ? `/ledger/transfers?${qs}` : "/ledger/transfers";
  return unwrap(request<ApiEnvelope<TransferPageResponse>>(path, init));
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
