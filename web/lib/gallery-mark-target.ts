import type { PlaybackMarkTarget } from "./api/playback";

export function galleryMarkTarget(mediaItemId: string): PlaybackMarkTarget {
  return {
    media_item_id: String(mediaItemId),
  };
}
