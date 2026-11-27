import type {
  NavigationMap,
  QueryMap,
  Receipt,
  SdkError,
  SdkErrorCode,
  TorrentView,
  UiHandle,
} from "@crawler-media/plugin-ui-sdk";

export function makeSdkError(
  code: SdkErrorCode,
  message: string,
  httpStatus?: number,
  fieldErrors?: Record<string, string>,
): SdkError {
  return { code, message, httpStatus, fieldErrors };
}

function normalizeFieldErrors(fields: unknown): Record<string, string> | undefined {
  if (!fields || typeof fields !== "object") {
    return undefined;
  }
  if (Array.isArray(fields)) {
    const out: Record<string, string> = {};
    for (const item of fields) {
      if (!item || typeof item !== "object") continue;
      const entry = item as { path?: unknown; message?: unknown; code?: unknown };
      const path = typeof entry.path === "string" ? entry.path : "";
      const text =
        (typeof entry.message === "string" && entry.message) ||
        (typeof entry.code === "string" && entry.code) ||
        "";
      if (path && text) {
        out[path] = text;
      }
    }
    return Object.keys(out).length > 0 ? out : undefined;
  }
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(fields as Record<string, unknown>)) {
    if (typeof v === "string") {
      out[k] = v;
    }
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

export function mapHttpToSdkError(
  status: number,
  apiCode: string | undefined,
  message: string,
  fields?: unknown,
): SdkError {
  const fieldErrors = normalizeFieldErrors(fields);
  if (
    apiCode === "plugin.stale_generation" ||
    apiCode === "plugin.stale_authority" ||
    apiCode === "plugin.package_conflict"
  ) {
    return makeSdkError("stale-generation", message, status, fieldErrors);
  }
  if (
    apiCode === "plugin.stale_handle" ||
    apiCode === "plugin.invalid_handle" ||
    apiCode === "plugin.cleanup_plan_expired"
  ) {
    return makeSdkError("stale-handle", message, status, fieldErrors);
  }
  if (apiCode === "plugin.revision_conflict" || apiCode === "plugin.conflict") {
    return makeSdkError("revision-conflict", message, status, fieldErrors);
  }
  if (apiCode === "plugin.operation_not_ready") {
    return makeSdkError("operation-not-ready", message, status, fieldErrors);
  }
  if (apiCode === "plugin.unknown_effect" || apiCode === "plugin.unknown_outcome") {
    return makeSdkError("result-unknown", message, status, fieldErrors);
  }
  if (
    status === 403 ||
    apiCode === "plugin.forbidden" ||
    apiCode === "plugin.permission_denied" ||
    apiCode === "plugin.not_enabled" ||
    apiCode === "plugin.capability_denied" ||
    apiCode === "plugin.ui_untrusted"
  ) {
    return makeSdkError("permission-denied", message, status, fieldErrors);
  }
  if (
    status === 429 ||
    status === 503 ||
    apiCode === "plugin.busy" ||
    apiCode === "plugin.not_drained" ||
    apiCode === "plugin.operation_active"
  ) {
    return makeSdkError("busy", message, status, fieldErrors);
  }
  if (status === 409 && !apiCode) {
    return makeSdkError("revision-conflict", message, status, fieldErrors);
  }
  return makeSdkError("request-failed", message, status, fieldErrors);
}

const VALID_RECEIPT_STATES = new Set<Receipt["state"]>([
  "completed",
  "already-completed",
  "auth-expired",
  "challenge",
  "unknown",
  "failed",
]);

export function normalizeReceiptState(rawOutcome: unknown, rawState: unknown): Receipt["state"] {
  if (rawOutcome === undefined && rawState === "succeeded") {
    return "completed";
  }
  const candidate = typeof rawOutcome === "string" && rawOutcome ? rawOutcome : rawState;
  if (candidate === "succeeded") {
    return "completed";
  }
  if (candidate === "rejected" || candidate === "cancelled") {
    return "failed";
  }
  if (typeof candidate === "string" && VALID_RECEIPT_STATES.has(candidate as Receipt["state"])) {
    return candidate as Receipt["state"];
  }
  return "unknown";
}

export function extractReceiptSiteId(r: Record<string, unknown>): string {
  if (typeof r.site_id === "string" && r.site_id) {
    return r.site_id;
  }
  if (Array.isArray(r.scope) && r.scope.length > 0) {
    const first = r.scope[0];
    if (typeof first === "string") return first;
    if (first && typeof first === "object") {
      const rec = first as Record<string, unknown>;
      if (typeof rec.Site === "string") return rec.Site;
      if (typeof rec.site_id === "string") return rec.site_id;
    }
  }
  return "";
}

export function normalizeQueryResult<K extends keyof QueryMap>(
  name: K,
  data: unknown,
): QueryMap[K]["result"] {
  if (
    name === "config.options" ||
    name === "downloaders.list" ||
    name === "sites.list" ||
    name === "libraries.list" ||
    name === "filters.list"
  ) {
    const page = (data ?? {}) as { items?: Array<Record<string, unknown>>; next_cursor?: string | null };
    return {
      items: (page.items ?? []).map((item) => ({
        ...item,
        id: String(item.id ?? ""),
        name: String(item.name ?? ""),
        capabilities: Array.isArray(item.capabilities) ? item.capabilities : [],
      })),
      next_cursor: page.next_cursor ?? null,
    } as QueryMap[K]["result"];
  }
  if (name === "checkin.receipts") {
    const page = (data ?? {}) as { items?: Array<Record<string, unknown>>; next_cursor?: string | null };
    return {
      items: (page.items ?? []).map((item) => ({
        site_id: extractReceiptSiteId(item),
        state: normalizeReceiptState(item.outcome, item.state),
        observed_at: String(item.observed_at ?? item.updated_at ?? ""),
        operation_id: typeof item.operation_id === "string" ? item.operation_id : null,
        detail:
          typeof item.detail === "string"
            ? item.detail
            : typeof item.evidence === "string"
              ? item.evidence
              : null,
      })),
      next_cursor: page.next_cursor ?? null,
    } as QueryMap[K]["result"];
  }
  if (name === "handles.resolve" || name === "files.selection") {
    const r = (data ?? {}) as Record<string, unknown>;
    return {
      ...r,
      handle: String(r.handle ?? "") as UiHandle,
      revision: String(r.revision ?? "0"),
      expires_at: String(r.expires_at ?? ""),
    } as QueryMap[K]["result"];
  }
  if (name === "boost.snapshot") {
    const r = (data ?? {}) as Record<string, unknown>;
    const rawDisk = (r.disk && typeof r.disk === "object" ? r.disk : {}) as Record<string, unknown>;
    return {
      ...r,
      revision: String(r.revision ?? "0"),
      used_bytes: typeof r.used_bytes === "number" ? r.used_bytes : null,
      reserved_bytes: typeof r.reserved_bytes === "number" ? r.reserved_bytes : null,
      retained_file_bytes: typeof r.retained_file_bytes === "number" ? r.retained_file_bytes : null,
      disk: {
        ...rawDisk,
        available_bytes: typeof rawDisk.available_bytes === "number" ? rawDisk.available_bytes : null,
        observed_at: rawDisk.observed_at == null ? null : String(rawDisk.observed_at),
        status: String(rawDisk.status ?? "unknown"),
      },
    } as QueryMap[K]["result"];
  }
  if (name === "media.get") {
    if (!data || typeof data !== "object") return data as QueryMap[K]["result"];
    const r = data as Record<string, unknown>;
    return {
      ...r,
      id: String(r.id ?? r.media_id ?? ""),
      title: String(r.title ?? r.name ?? ""),
      revision: String(r.revision ?? "1"),
    } as QueryMap[K]["result"];
  }
  if (name === "subscribes.list") {
    const page = (data ?? {}) as { items?: Array<Record<string, unknown>>; next_cursor?: string | null };
    return {
      items: (page.items ?? []).map((item) => ({
        ...item,
        id: String(item.id ?? ""),
        media_id: String(item.media_id ?? ""),
        revision: String(item.revision ?? "1"),
        title: String(item.title ?? item.name ?? item.media_id ?? item.id ?? ""),
      })),
      next_cursor: page.next_cursor ?? null,
    } as QueryMap[K]["result"];
  }
  if (name === "downloads.list") {
    const page = (data ?? {}) as { items?: Array<Record<string, unknown>>; next_cursor?: string | null };
    return {
      items: (page.items ?? []).map((item) => ({
        ...item,
        id: String(item.id ?? ""),
        name: String(item.name ?? item.title ?? item.info_hash ?? item.id ?? ""),
        revision: String(item.revision ?? "0"),
        state: String(item.state ?? "unknown"),
        site_id: typeof item.site_id === "string" ? item.site_id : null,
        downloader_id: String(item.downloader_id ?? ""),
      })),
      next_cursor: page.next_cursor ?? null,
    } as QueryMap[K]["result"];
  }
  if (name === "indexer.cached") {
    const page = (data ?? {}) as { items?: TorrentView[]; next_cursor?: string | null };
    return {
      items: Array.isArray(page.items) ? page.items : [],
      next_cursor: page.next_cursor ?? null,
    } as QueryMap[K]["result"];
  }
  return data as QueryMap[K]["result"];
}

export function resolveRoutePath<K extends keyof NavigationMap>(
  pluginIdEnc: string,
  route: K,
  params: NavigationMap[K],
): string {
  switch (route) {
    case "library":
      return `/library/${encodeURIComponent((params as NavigationMap["library"]).library_id)}`;
    case "media": {
      const p = (params ?? {}) as NavigationMap["media"];
      if (p.library_id && p.media_id) {
        return `/library/${encodeURIComponent(p.library_id)}/item/${encodeURIComponent(p.media_id)}`;
      }
      if ((p.kind === "movie" || p.kind === "tv") && p.tmdb_id) {
        return `/media/${p.kind}/${encodeURIComponent(p.tmdb_id)}`;
      }
      if (p.douban_id) {
        return `/media/douban/${encodeURIComponent(p.douban_id)}`;
      }
      return `/media/item/${encodeURIComponent(p.media_id ?? "")}`;
    }
    case "search": {
      const q = (params as NavigationMap["search"]).query;
      return q ? `/search?q=${encodeURIComponent(q)}` : "/search";
    }
    case "plugin": {
      const surfaceId = (params as NavigationMap["plugin"]).surface_id;
      return `/plugin-apps/${pluginIdEnc}/${encodeURIComponent(surfaceId)}`;
    }
  }
}
