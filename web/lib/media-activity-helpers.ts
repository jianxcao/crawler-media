/**
 * 媒体库实时活动视图共用的纯格式化与目标跳转推导。
 */

import type { MediaActivityTarget } from "./api/playback.ts";
import { formatBytes } from "./format.ts";

export function formatRate(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

/** 毫秒 → 播放器习惯的 h:mm:ss / m:ss。 */
export function formatPlayClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const two = (v: number) => String(v).padStart(2, "0");
  return hours > 0 ? `${hours}:${two(minutes)}:${two(seconds)}` : `${minutes}:${two(seconds)}`;
}

export function unitLabel(
  media: Pick<MediaActivityTarget, "kind" | "season_number" | "episode_number">,
): string | null {
  if (media.kind !== "tv") return null;
  const season = String(media.season_number).padStart(2, "0");
  const episode = String(media.episode_number).padStart(2, "0");
  return `S${season}E${episode}`;
}

export function deviceLabel(client: string, deviceName: string): string {
  if (client && deviceName && client !== deviceName) return `${client} · ${deviceName}`;
  return deviceName || client || "未知设备";
}

export function detailHref(
  media: Pick<MediaActivityTarget, "library_id" | "media_item_id" | "browsable">,
): string | null {
  if (media.library_id == null || !media.browsable) return null;
  return `/library/${media.library_id}/item/${media.media_item_id}`;
}
