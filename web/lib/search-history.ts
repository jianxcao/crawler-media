import type { MediaSearchItem } from "./api/discover.ts";
import type { MediaSource, MediaType } from "./media-types.ts";

export interface SearchHistoryIdentity {
  id: string;
  provider: string | null;
}

/** History provider values are vertical markers: torrents for site search, titles for catalog search. */
export function isMediaHistoryProvider(provider: string | null): boolean {
  return provider != null && provider !== "torrents";
}

/** Re-open a history entry from its stored result snapshot. */
export function historyReplayOptions(
  item: SearchHistoryIdentity,
): { vertical: "media" | "torrent"; snapshotId: string } {
  return {
    vertical: isMediaHistoryProvider(item.provider) ? "media" : "torrent",
    snapshotId: item.id,
  };
}

export interface TitleHistoryHit {
  provider: string;
  external_id: string;
  kind: MediaType;
  title: string;
  year: number | null;
  original_title: string | null;
  poster_url: string | null;
}

/** Convert the persisted title hit DTO into the frontend media result shape. */
export function toMediaSearchItem(
  hit: TitleHistoryHit,
  mapPosterUrl: (url: string) => string,
): MediaSearchItem {
  return {
    id: hit.external_id,
    source: hit.provider as MediaSource,
    title: hit.title,
    year: hit.year ?? undefined,
    type: hit.kind,
    rating: 0,
    posterUrl: hit.poster_url ? mapPosterUrl(hit.poster_url) : "",
  };
}
