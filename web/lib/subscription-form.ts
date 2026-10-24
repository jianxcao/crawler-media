import type { CreateSubscriptionPayload } from "./api/subscriptions";

export function createSubscriptionBody(payload: CreateSubscriptionPayload): Record<string, unknown> {
  const body: Record<string, unknown> = {
    title_ref: payload.title_ref,
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
