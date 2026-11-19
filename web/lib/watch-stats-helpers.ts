/**
 * 观看统计（WatchStatsPanel）所需的纯数值归约、坐标刻度和排行榜比较逻辑。
 */

import type { PlaybackStatsDayRow, PlaybackStatsTitleRow } from "./api/playback.ts";

export type MetricKey = "watched_ms" | "plays" | "completion" | "active_members";

export const SERIES_COLOR = "var(--info)";
export const PREVIOUS_COLOR = "rgba(255,255,255,0.32)";
export const WEEKDAYS = ["一", "二", "三", "四", "五", "六", "日"];

/** 从末尾往前每 size 天折成一桶，保证最新一桶是完整的（size=1 即不折）。 */
export function bucketize(
  rows: PlaybackStatsDayRow[],
  size: number,
): PlaybackStatsDayRow[][] {
  const out: PlaybackStatsDayRow[][] = [];
  for (let end = rows.length; end > 0; end -= size) {
    out.unshift(rows.slice(Math.max(0, end - size), end));
  }
  return out;
}

export function dayLabel(date: string): string {
  const [, month, day] = date.split("-");
  return `${Number(month)}月${Number(day)}日`;
}

export function bucketLabel(rows: PlaybackStatsDayRow[]): string {
  if (rows.length === 1) return dayLabel(rows[0].date);
  return `${dayLabel(rows[0].date)} – ${dayLabel(rows[rows.length - 1].date)}`;
}

/** 顶部圆角的柱状路径：圆角只在数据端，底边贴着基线。 */
export function barPath(x: number, y: number, w: number, h: number): string {
  const r = Math.min(3, w / 2, h);
  if (h <= 0) return "";
  return [
    `M${x.toFixed(1)},${(y + h).toFixed(1)}`,
    `V${(y + r).toFixed(1)}`,
    `Q${x.toFixed(1)},${y.toFixed(1)} ${(x + r).toFixed(1)},${y.toFixed(1)}`,
    `H${(x + w - r).toFixed(1)}`,
    `Q${(x + w).toFixed(1)},${y.toFixed(1)} ${(x + w).toFixed(1)},${(y + r).toFixed(1)}`,
    `V${(y + h).toFixed(1)} Z`,
  ].join(" ");
}

/** 坐标轴的整齐上限：1 / 2 / 5 × 10^n 里第一个不小于最大值的数。 */
export function niceMax(max: number): number {
  if (max <= 0) return 1;
  const exponent = Math.floor(Math.log10(max));
  const base = Math.pow(10, exponent);
  for (const step of [1, 2, 5, 10]) {
    if (max <= step * base) return step * base;
  }
  return 10 * base;
}

export function computeDeltaChip({
  current,
  previous,
  available,
  inPoints,
}: {
  current: number;
  previous: number;
  available: boolean;
  inPoints?: boolean;
}): {
  text: string;
  direction: 1 | 0 | -1;
  tone: string;
  arrow: string;
} | null {
  if (!available) return null;
  let text: string;
  let direction: 1 | 0 | -1;
  if (inPoints) {
    const points = Math.round((current - previous) * 100);
    direction = points > 0 ? 1 : points < 0 ? -1 : 0;
    text = `${Math.abs(points)} 个百分点`;
  } else if (previous <= 0) {
    if (current <= 0) return null;
    direction = 1;
    text = "新增";
  } else {
    const ratio = (current - previous) / previous;
    direction = ratio > 0 ? 1 : ratio < 0 ? -1 : 0;
    text = direction === 0 ? "持平" : `${Math.round(Math.abs(ratio) * 100)}%`;
  }
  const tone =
    direction > 0
      ? "bg-[var(--ok)]/15 text-[var(--ok)]"
      : direction < 0
        ? "bg-[var(--danger)]/15 text-[var(--danger)]"
        : "bg-white/[0.08] text-white/55";
  const arrow = direction > 0 ? "▲" : direction < 0 ? "▼" : "";
  return { text, direction, tone, arrow };
}

/** 与上一周期前三的对照：同名次「蝉联」，换了名次「上期第 n」，上期不在榜「新上榜」。 */
export function rankChange(
  row: PlaybackStatsTitleRow,
  rank: number,
  previous: PlaybackStatsTitleRow[],
): { text: string; tone: string } | null {
  if (previous.length === 0) return null;
  const was = previous.findIndex((p) => p.media.media_item_id === row.media.media_item_id);
  if (was === rank) return { text: "蝉联", tone: "bg-[var(--ok)]/15 text-[var(--ok)]" };
  if (was === -1) return { text: "新上榜", tone: "bg-[var(--info)]/15 text-[var(--info)]" };
  return {
    text: `${was > rank ? "▲" : "▼"} 上期第 ${was + 1}`,
    tone: was > rank ? "bg-[var(--ok)]/15 text-[var(--ok)]" : "bg-white/[0.08] text-white/55",
  };
}
