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

export interface VirtualRangeInput {
  count: number;
  scrollTop: number;
  viewportHeight: number;
  listOffsetTop?: number;
  overscan?: number;
  defaultItemHeight?: number;
  gap?: number;
  getItemHeight?: (index: number) => number | undefined;
}

export interface VirtualRangeResult {
  startIndex: number;
  endIndex: number;
  visibleCount: number;
  topSpacerPx: number;
  bottomSpacerPx: number;
  totalHeightPx: number;
}

/**
 * Window-scroll virtualizer range calculator using prefix offsets + binary search.
 * Preserves `topSpacerPx + renderedSliceHeight + bottomSpacerPx === totalHeightPx`
 * across all scroll positions and variable measured heights.
 */
export function computeVirtualRange(input: VirtualRangeInput): VirtualRangeResult {
  const { count } = input;
  if (count <= 0) {
    return {
      startIndex: 0,
      endIndex: 0,
      visibleCount: 0,
      topSpacerPx: 0,
      bottomSpacerPx: 0,
      totalHeightPx: 0,
    };
  }

  const defaultHeight = Math.max(1, input.defaultItemHeight ?? 148);
  const gap = Math.max(0, input.gap ?? 12);
  const overscan = Math.max(0, input.overscan ?? 6);
  const viewportHeight = Math.max(1, input.viewportHeight || 900);
  const listOffsetTop = input.listOffsetTop ?? 0;

  const offsets = new Float64Array(count);
  const heights = new Float64Array(count);
  let cursor = 0;
  for (let i = 0; i < count; i += 1) {
    const raw = input.getItemHeight?.(i);
    const h = raw != null && raw > 0 ? raw : defaultHeight;
    offsets[i] = cursor;
    heights[i] = h;
    cursor += h + (i < count - 1 ? gap : 0);
  }
  const totalHeightPx = cursor;

  const relTop = Math.max(0, input.scrollTop - listOffsetTop);
  const relBottom = Math.max(relTop, input.scrollTop + viewportHeight - listOffsetTop);

  let firstVisible: number;
  if (relTop >= totalHeightPx) {
    firstVisible = count - 1;
  } else {
    let lo = 0;
    let hi = count - 1;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (offsets[mid] + heights[mid] <= relTop) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    firstVisible = lo;
  }

  let lastVisibleExclusive: number;
  if (relBottom <= 0) {
    lastVisibleExclusive = 1;
  } else if (relBottom >= totalHeightPx) {
    lastVisibleExclusive = count;
  } else {
    let lo = firstVisible;
    let hi = count;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (offsets[mid] < relBottom) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    lastVisibleExclusive = Math.max(firstVisible + 1, lo);
  }

  const startIndex = Math.max(0, firstVisible - overscan);
  const endIndex = Math.min(count, lastVisibleExclusive + overscan);
  const visibleCount = endIndex - startIndex;
  const topSpacerPx = offsets[startIndex];
  const lastRendered = endIndex - 1;
  const bottomSpacerPx = totalHeightPx - (offsets[lastRendered] + heights[lastRendered]);

  return {
    startIndex,
    endIndex,
    visibleCount,
    topSpacerPx,
    bottomSpacerPx,
    totalHeightPx,
  };
}

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
