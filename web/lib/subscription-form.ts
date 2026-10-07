import type { CreateSubscriptionPayload } from "./api/subscriptions";

export function createSubscriptionBody(payload: CreateSubscriptionPayload): Record<string, unknown> {
  const body: Record<string, unknown> = {
    title_ref: payload.title_ref,
    ...(payload.follow_future != null ? { follow_future: payload.follow_future } : {}),
    ...(payload.rule_set_id != null ? { filter_id: String(payload.rule_set_id) } : {}),
    ...(payload.library_id != null ? { library_id: String(payload.library_id) } : {}),
    ...(payload.wash_cut != null ? { wash_cut: payload.wash_cut } : {}),
    ...(payload.wash_cut_filter_id != null ? { wash_cut_filter_id: String(payload.wash_cut_filter_id) } : {}),
    ...(payload.keep_old_versions != null ? { keep_old_versions: payload.keep_old_versions } : {}),
  };
  const seasons = (payload.selected_seasons ?? [])
    .filter((s) => s > 0)
    .sort((a, b) => a - b);
  if (seasons.length > 0) {
    body.coverage = { kind: "tv", season: seasons[0], episode_from: 1 };
  }
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
