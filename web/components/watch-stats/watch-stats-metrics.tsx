"use client";

import { useLayoutEffect, useRef, useState } from "react";

import { formatWatched } from "@/components/playback-stats-section";
import type { PlaybackStatsDayRow, PlaybackWatchStats } from "@/lib/api/playback";
import {
  PREVIOUS_COLOR,
  SERIES_COLOR,
  barPath,
  bucketLabel,
  bucketize,
  computeDeltaChip,
  dayLabel,
  niceMax,
  type MetricKey,
} from "@/lib/watch-stats-helpers";

export interface MetricDef {
  key: MetricKey;
  label: string;
  ofDays: (rows: PlaybackStatsDayRow[]) => number;
  ofTotals: (t: PlaybackWatchStats["current"]) => number;
  format: (value: number) => string;
  parts: (value: number) => [string, string];
  axis: (value: number) => string;
  deltaInPoints?: boolean;
  bucketNote?: string;
}

const sum = (rows: PlaybackStatsDayRow[], pick: (d: PlaybackStatsDayRow) => number) =>
  rows.reduce((acc, d) => acc + pick(d), 0);

const hours = (ms: number) => (ms / 3_600_000).toFixed(ms >= 360_000_000 ? 0 : 1);

export const METRICS: readonly MetricDef[] = [
  {
    key: "watched_ms",
    label: "观看时长",
    ofDays: (rows) => sum(rows, (d) => d.watched_ms),
    ofTotals: (t) => t.watched_ms,
    format: (v) => (v >= 3_600_000 ? `${hours(v)} 小时` : formatWatched(v)),
    parts: (v) =>
      v >= 3_600_000
        ? [hours(v), "小时"]
        : [`${Math.max(v > 0 ? 1 : 0, Math.round(v / 60_000))}`, "分钟"],
    axis: (v) => `${(v / 3_600_000).toFixed(v >= 36_000_000 ? 0 : 1)}h`,
  },
  {
    key: "plays",
    label: "播放场次",
    ofDays: (rows) => sum(rows, (d) => d.plays),
    ofTotals: (t) => t.plays,
    format: (v) => `${v} 场`,
    parts: (v) => [`${v}`, "场"],
    axis: (v) => `${Math.round(v)}`,
  },
  {
    key: "completion",
    label: "看完率",
    ofDays: (rows) => {
      const plays = sum(rows, (d) => d.plays);
      return plays > 0 ? sum(rows, (d) => d.completed) / plays : 0;
    },
    ofTotals: (t) => (t.plays > 0 ? t.completed / t.plays : 0),
    format: (v) => `${Math.round(v * 100)}%`,
    parts: (v) => [`${Math.round(v * 100)}`, "%"],
    axis: (v) => `${Math.round(v * 100)}%`,
    deltaInPoints: true,
  },
  {
    key: "active_members",
    label: "活跃成员",
    ofDays: (rows) => (rows.length > 0 ? sum(rows, (d) => d.members) / rows.length : 0),
    ofTotals: (t) => t.active_members,
    format: (v) => `${Math.round(v * 10) / 10} 人`,
    parts: (v) => [`${Math.round(v * 10) / 10}`, "人"],
    axis: (v) => `${Math.round(v)}`,
    bucketNote: "日均",
  },
] as const;

function useElementWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T | null>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

export function DeltaChip({
  current,
  previous,
  available,
  inPoints,
  title,
}: {
  current: number;
  previous: number;
  available: boolean;
  inPoints?: boolean;
  title: string;
}) {
  const delta = computeDeltaChip({ current, previous, available, inPoints });
  if (!delta) return null;
  return (
    <span
      title={title}
      className={`tnum inline-flex shrink-0 items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[11px] font-semibold leading-4 ${delta.tone}`}
    >
      {delta.arrow && <span className="text-[9px]">{delta.arrow}</span>}
      {delta.text}
    </span>
  );
}

export function MetricCard({
  metric,
  stats,
  selected,
  onSelect,
}: {
  metric: MetricDef;
  stats: PlaybackWatchStats;
  selected: boolean;
  onSelect: () => void;
}) {
  const current = metric.ofTotals(stats.current);
  const previous = metric.ofTotals(stats.previous);
  const [value, unit] = metric.parts(current);
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onSelect}
      className={`rounded-2xl border px-4 py-3.5 text-left transition max-md:px-3.5 ${
        selected
          ? "border-[var(--info)]/60 bg-[var(--info)]/[0.07]"
          : "border-white/[0.08] bg-white/[0.03] hover:border-white/[0.14] hover:bg-white/[0.05]"
      }`}
    >
      <p className="truncate text-caption font-medium text-white/55">{metric.label}</p>
      <p className="mt-2 whitespace-nowrap leading-none">
        <span className="tnum text-[26px] font-bold tracking-tight text-white">{value}</span>
        <span className="ml-1 text-sub font-medium text-white/45">{unit}</span>
      </p>
      <div className="mt-2.5 flex min-w-0 items-center gap-2">
        <DeltaChip
          current={current}
          previous={previous}
          available={stats.previous_available}
          inPoints={metric.deltaInPoints}
          title="较上一周期"
        />
        <span className="tnum truncate text-[11px] text-white/35">
          {stats.previous_available ? `上期 ${metric.format(previous)}` : "暂无上一周期数据"}
        </span>
      </div>
    </button>
  );
}

const CHART_HEIGHT = 220;
const PAD = { top: 14, right: 12, bottom: 26, left: 46 };
const MIN_SLOT = 6;

export function TrendChart({ stats, metric }: { stats: PlaybackWatchStats; metric: MetricDef }) {
  const [ref, width] = useElementWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const innerWidth = Math.max(0, width - PAD.left - PAD.right);
  const innerHeight = CHART_HEIGHT - PAD.top - PAD.bottom;
  const weekly = stats.by_day.length > 0 && innerWidth / stats.by_day.length < MIN_SLOT;
  const buckets = bucketize(stats.by_day, weekly ? 7 : 1);
  const prevBuckets = bucketize(stats.previous_by_day, weekly ? 7 : 1);
  const current = buckets.map(metric.ofDays);
  const previous = prevBuckets.map(metric.ofDays);
  const showPrevious = stats.previous_available && previous.length === current.length;
  const count = current.length;
  const yMax = niceMax(Math.max(...current, ...(showPrevious ? previous : [0])));
  const slot = count > 0 ? innerWidth / count : 0;
  const gap = Math.min(6, Math.max(1, slot * 0.3));
  const barWidth = Math.max(1, slot - gap);
  const cx = (i: number) => PAD.left + slot * i + slot / 2;
  const y = (v: number) => PAD.top + innerHeight - (v / yMax) * innerHeight;
  const baseline = PAD.top + innerHeight;
  const previousLine = previous
    .map((v, i) => `${i === 0 ? "M" : "L"}${cx(i).toFixed(1)},${y(v).toFixed(1)}`)
    .join(" ");
  const gridSteps = 4;
  const tickTarget = Math.max(2, Math.min(6, Math.floor(innerWidth / 70)));
  const tickEvery = Math.max(1, Math.round(count / tickTarget));
  const ticks = buckets.map((_, i) => i).filter((i) => i % tickEvery === 0 || i === count - 1);
  const bucketNote = weekly && metric.bucketNote ? `${metric.bucketNote} ` : "";

  const onMove = (event: React.MouseEvent<SVGSVGElement>) => {
    if (slot <= 0) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const px = event.clientX - rect.left - PAD.left;
    setHover(Math.max(0, Math.min(count - 1, Math.floor(px / slot))));
  };

  return (
    <div ref={ref} className="relative">
      <p className="mb-1 px-1 text-caption font-semibold text-white/55">
        {metric.label}
        <span className="ml-1.5 font-normal text-white/35">{weekly ? "按周" : "按天"}</span>
      </p>
      {width > 0 && (
        <svg
          width={width}
          height={CHART_HEIGHT}
          role="img"
          aria-label={`${metric.label}走势`}
          onMouseMove={onMove}
          onMouseLeave={() => setHover(null)}
        >
          {Array.from({ length: gridSteps + 1 }, (_, i) => {
            const v = (yMax / gridSteps) * i;
            const yy = y(v);
            return (
              <g key={i}>
                <line
                  x1={PAD.left}
                  x2={width - PAD.right}
                  y1={yy}
                  y2={yy}
                  stroke={i === 0 ? "var(--line-strong)" : "var(--line)"}
                />
                <text
                  x={PAD.left - 8}
                  y={yy + 3.5}
                  textAnchor="end"
                  fontSize={10}
                  fill="var(--text-faint)"
                  className="tnum"
                >
                  {metric.axis(v)}
                </text>
              </g>
            );
          })}
          {ticks.map((i) => (
            <text
              key={i}
              x={cx(i)}
              y={CHART_HEIGHT - 8}
              textAnchor={i === 0 ? "start" : i === count - 1 ? "end" : "middle"}
              fontSize={10}
              fill="var(--text-faint)"
            >
              {dayLabel(buckets[i][0].date)}
            </text>
          ))}
          {current.map((v, i) =>
            v > 0 ? (
              <path
                key={i}
                d={barPath(cx(i) - barWidth / 2, y(v), barWidth, baseline - y(v))}
                fill={SERIES_COLOR}
                opacity={hover === null || hover === i ? 0.95 : 0.55}
              />
            ) : null,
          )}
          {showPrevious && (
            <path
              d={previousLine}
              fill="none"
              stroke={PREVIOUS_COLOR}
              strokeWidth={1.5}
              strokeDasharray="4 4"
              strokeLinejoin="round"
            />
          )}
          {hover !== null && (
            <g>
              <rect
                x={cx(hover) - slot / 2}
                y={PAD.top}
                width={slot}
                height={innerHeight}
                fill="var(--glass-fill-hover)"
              />
              {showPrevious && (
                <circle
                  cx={cx(hover)}
                  cy={y(previous[hover])}
                  r={3.5}
                  fill={PREVIOUS_COLOR}
                  stroke="var(--bg)"
                  strokeWidth={1.5}
                />
              )}
            </g>
          )}
        </svg>
      )}
      {width > 0 && current.every((v) => v <= 0) && (
        <p className="pointer-events-none absolute inset-x-0 top-[44px] text-center">
          <span className="popover-surface inline-block rounded-full px-3 py-1 text-caption text-[var(--text-muted)]">
            本周期没有播放{showPrevious ? "，虚线是上一周期" : ""}
          </span>
        </p>
      )}
      {hover !== null && width > 0 && (
        <div
          className="pointer-events-none absolute z-10"
          style={{
            left: Math.min(Math.max(cx(hover) - 80, 0), width - 170),
            top: 24,
          }}
        >
          <div className="menu-surface min-w-[10rem] px-3 py-2 text-caption">
            <p className="font-semibold text-white/85">
              {bucketLabel(buckets[hover])}
              {weekly && buckets[hover].length < 7 && (
                <span className="ml-1 font-normal text-white/35">{buckets[hover].length} 天</span>
              )}
            </p>
            <p className="tnum mt-1 flex items-center gap-1.5 text-white/80">
              <span
                className="inline-block size-2 rounded-[2px]"
                style={{ background: SERIES_COLOR }}
              />
              本周期 {bucketNote}
              {metric.format(current[hover])}
            </p>
            {showPrevious && (
              <p className="tnum mt-0.5 flex items-center gap-1.5 text-white/55">
                <span
                  className="inline-block h-0 w-2 border-t border-dashed"
                  style={{ borderColor: PREVIOUS_COLOR }}
                />
                上一周期 {bucketNote}
                {metric.format(previous[hover])}
                <span className="text-white/35">{bucketLabel(prevBuckets[hover])}</span>
              </p>
            )}
          </div>
        </div>
      )}
      <div className="mt-1 flex items-center gap-4 text-caption text-white/45">
        <span className="flex items-center gap-1.5">
          <span
            className="inline-block size-2.5 rounded-[3px]"
            style={{ background: SERIES_COLOR }}
          />
          本周期
        </span>
        {showPrevious && (
          <span className="flex items-center gap-1.5">
            <span
              className="inline-block h-0 w-4 border-t-2 border-dashed"
              style={{ borderColor: PREVIOUS_COLOR }}
            />
            上一周期
          </span>
        )}
      </div>
    </div>
  );
}
