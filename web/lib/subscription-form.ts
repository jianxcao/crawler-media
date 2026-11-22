import type { CreateSubscriptionPayload, PipelineHealth } from "./api/subscriptions";

export function createSubscriptionBody(payload: CreateSubscriptionPayload): Record<string, unknown> {
  const body: Record<string, unknown> = {
    title_ref: payload.title_ref,
    ...(payload.source_title_ref ? { source_title_ref: payload.source_title_ref } : {}),
    ...(payload.follow_future != null ? { follow_future: payload.follow_future } : {}),
    ...(payload.filter_id != null ? { filter_id: String(payload.filter_id) } : {}),
    ...(payload.library_id != null ? { library_id: String(payload.library_id) } : {}),
    ...(payload.wash_cut != null ? { wash_cut: payload.wash_cut } : {}),
    ...(payload.wash_cut_filter_id != null ? { wash_cut_filter_id: String(payload.wash_cut_filter_id) } : {}),
    ...(payload.keep_old_versions != null ? { keep_old_versions: payload.keep_old_versions } : {}),
  };
  if (payload.coverage) body.coverage = payload.coverage;
  return body;
}

export function describeSubscriptionHealthBanner(
  health: PipelineHealth | null | undefined,
): string | null {
  if (!health || health.status !== "error") return null;
  if (!health.sites_configured && !health.downloaders_configured) {
    return "尚未接入资源站点与下载器，订阅暂无法自动搜索和下载入库";
  }
  if (!health.sites_configured) {
    return "尚未启用任何资源站点，订阅暂无法自动搜索资源";
  }
  if (!health.downloaders_configured) {
    return "尚未配置默认下载器，订阅暂无法自动投递下载";
  }
  const libErrors =
    health.library_error_count ??
    (health.libraries ?? []).filter((l) => l.status === "error").length;
  if (libErrors > 0) {
    return `${libErrors} 个媒体库的入库链路有问题，相关订阅暂时无法自动下载入库（已下达的任务会自动重试）`;
  }
  return "订阅入库链路存在配置问题，暂时无法自动下载入库（已下达的任务会自动重试）";
}

export function isLibraryDirty(initialLibraryId: string | null, currentLibraryId: string | null): boolean {
  const init = initialLibraryId ?? null;
  const curr = currentLibraryId ?? null;
  return init !== curr;
}

export function libraryPatch(
  initialLibraryId: string | null,
  currentLibraryId: string | null,
): { library_id?: string | null } {
  if (!isLibraryDirty(initialLibraryId, currentLibraryId)) {
    return {};
  }
  return { library_id: currentLibraryId ?? null };
}
