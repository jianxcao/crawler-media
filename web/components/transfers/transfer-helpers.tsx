import type { TransferMode, TransferSummary } from "@/lib/api/transfers";

export const EMPTY_TRANSFER_SUMMARY: TransferSummary = {
  total_files: 0,
  media_count: 0,
  movie_files: 0,
  tv_files: 0,
  strm_count: 0,
  hardlink_count: 0,
  copy_count: 0,
  move_count: 0,
  local_size_bytes: 0,
  probed_duration_ms: 0,
  strm_probed_count: 0,
};

export function formatSummaryDuration(durationMs: number): string {
  if (!Number.isFinite(durationMs) || durationMs <= 0) {
    return "云端按需直连";
  }
  const totalMinutes = Math.max(1, Math.round(durationMs / 60_000));
  if (totalMinutes < 60) {
    return `${totalMinutes.toLocaleString("zh-CN")} 分钟`;
  }
  const exactHours = durationMs / 3_600_000;
  if (exactHours >= 10) {
    return `${Math.round(exactHours).toLocaleString("zh-CN")} 小时`;
  }
  const hours = Math.floor(totalMinutes / 60);
  const rem = totalMinutes % 60;
  return rem > 0 ? `${hours} 小时 ${rem} 分钟` : `${hours} 小时`;
}

export function formatItemDuration(durationMs: number | null | undefined): string | null {
  if (durationMs == null || !Number.isFinite(durationMs) || durationMs <= 0) {
    return null;
  }
  const totalMinutes = Math.max(1, Math.round(durationMs / 60_000));
  if (totalMinutes < 60) {
    return `${totalMinutes} 分钟`;
  }
  const hours = Math.floor(totalMinutes / 60);
  const rem = totalMinutes % 60;
  if (hours >= 100) {
    return `${hours.toLocaleString("zh-CN")} 小时`;
  }
  return rem > 0 ? `${hours} 小时 ${rem} 分` : `${hours} 小时`;
}

export function formatEpisodeTag(
  units: Array<{ season: number; episode: number }>,
): string | null {
  const tags = units
    .filter((unit) => unit.season >= 0 && unit.episode >= 0)
    .map(
      (unit) =>
        `S${String(unit.season).padStart(2, "0")}E${String(unit.episode).padStart(2, "0")}`,
    );
  if (tags.length === 0) return null;
  if (tags.length <= 3) return tags.join(" · ");
  return `${tags[0]}–${tags[tags.length - 1]}`;
}

export function transferModeBadgeMeta(mode: TransferMode): {
  label: string;
  shortLabel: string;
  className: string;
} {
  switch (mode) {
    case "strm":
      return {
        label: "STRM 直链",
        shortLabel: "STRM",
        className: "bg-purple-500/15 text-purple-300 border border-purple-500/25",
      };
    case "hardlink":
      return {
        label: "硬链接 (Hardlink)",
        shortLabel: "硬链接",
        className: "bg-emerald-500/15 text-emerald-300 border border-emerald-500/25",
      };
    case "move":
      return {
        label: "移动 (Move)",
        shortLabel: "移动",
        className: "bg-sky-500/15 text-sky-300 border border-sky-500/25",
      };
    case "copy":
    default:
      return {
        label: "复制 (Copy)",
        shortLabel: "复制",
        className: "bg-amber-500/15 text-amber-300 border border-amber-500/25",
      };
  }
}
