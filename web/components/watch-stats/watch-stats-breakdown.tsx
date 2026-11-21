"use client";

import { useState } from "react";

import { formatWatched } from "@/components/playback-stats-section";
import { SERIES_COLOR, WEEKDAYS } from "@/lib/watch-stats-helpers";

export interface BreakdownRow {
  key: string;
  label: React.ReactNode;
  value: number;
  valueLabel: string;
  secondary?: string;
  leading?: React.ReactNode;
  onSelect?: () => void;
  selected?: boolean;
  muted?: boolean;
}

export function BreakdownPanel({
  title,
  note,
  rows,
  total,
  footer,
  emptyText = "本周期没有数据",
  unit,
  formatValue,
  fold = "other",
  limit = 5,
}: {
  title: string;
  note?: string;
  rows: BreakdownRow[];
  total: number;
  footer?: React.ReactNode;
  emptyText?: string | null;
  unit: string;
  formatValue: (value: number) => string;
  fold?: "other" | "expand";
  limit?: number;
}) {
  const [expanded, setExpanded] = useState(false);
  let visible = rows;
  let tail: React.ReactNode = null;
  if (rows.length > limit) {
    if (fold === "expand") {
      visible = expanded ? rows : rows.slice(0, limit);
      tail = (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="w-full px-4 py-2.5 text-left text-caption font-medium text-[var(--info)] transition hover:text-white max-md:px-3.5"
        >
          {expanded ? "收起" : `展开全部 ${rows.length} ${unit}`}
        </button>
      );
    } else {
      const rest = rows.slice(limit);
      const value = rest.reduce((acc, r) => acc + r.value, 0);
      visible = [
        ...rows.slice(0, limit),
        {
          key: "__other",
          label: `其他 ${rest.length} ${unit}`,
          value,
          valueLabel: formatValue(value),
          muted: true,
        },
      ];
    }
  }
  return (
    <section aria-label={title} className="flex h-full flex-col">
      <div className="mb-1.5 flex items-baseline gap-2">
        <p className="text-caption font-semibold text-white/55">{title}</p>
        {note && <p className="text-[11px] text-white/30">{note}</p>}
      </div>
      <div className="flex flex-1 flex-col divide-y divide-white/[0.06] rounded-2xl border border-white/[0.08] bg-white/[0.02]">
        {rows.length === 0 && emptyText && (
          <p className="px-4 py-3 text-caption text-white/40">{emptyText}</p>
        )}
        {visible.map((row) => {
          const share = total > 0 ? Math.round((row.value / total) * 100) : 0;
          const body = (
            <>
              {row.leading}
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline justify-between gap-3">
                  <div className="flex min-w-0 items-baseline gap-2">
                    <div
                      className={`min-w-0 text-ui font-medium ${row.muted ? "text-white/50" : "text-white/85"}`}
                    >
                      {row.label}
                    </div>
                    {row.secondary && (
                      <span className="tnum shrink-0 text-[11px] text-white/35">
                        {row.secondary}
                      </span>
                    )}
                  </div>
                  <div className="tnum shrink-0 text-caption text-white/70">
                    {row.valueLabel}
                    <span className="ml-1.5 text-white/35">{share}%</span>
                  </div>
                </div>
                <div className="mt-1.5 h-[3px] rounded-full bg-white/[0.06]">
                  <div
                    className="h-full rounded-full"
                    style={{
                      width: `${Math.max(1, share)}%`,
                      background: row.selected
                        ? "var(--ok)"
                        : row.muted
                          ? "rgba(255,255,255,0.22)"
                          : SERIES_COLOR,
                      opacity: row.selected || row.muted ? 1 : 0.9,
                    }}
                  />
                </div>
              </div>
            </>
          );
          const className = "flex items-center gap-3 px-4 py-2.5 max-md:px-3.5";
          return row.onSelect ? (
            <button
              key={row.key}
              type="button"
              aria-pressed={row.selected}
              onClick={row.onSelect}
              className={`${className} w-full text-left transition hover:bg-white/[0.04]`}
            >
              {body}
            </button>
          ) : (
            <div key={row.key} className={className}>
              {body}
            </div>
          );
        })}
        {tail}
        {footer}
      </div>
    </section>
  );
}

/** 星期 × 小时观看时段热力图。 */
export function HourHeatmap({ matrix }: { matrix: number[][] }) {
  const max = Math.max(1e-9, ...matrix.flat());
  const total = matrix.flat().reduce((a, b) => a + b, 0);
  return (
    <section aria-label="观看时段">
      <div className="mb-1.5 flex items-baseline gap-2">
        <p className="text-caption font-semibold text-white/55">观看时段</p>
        <p className="text-[11px] text-white/30">星期 × 小时的观看时长，越深越多</p>
      </div>
      <div className="overflow-x-auto rounded-2xl border border-white/[0.08] bg-white/[0.02] px-4 py-3">
        {total === 0 && (
          <p className="mb-2 text-caption text-white/40">
            本周期还没有累计到观看时长；有人看过之后这里会显示星期 × 小时的分布
          </p>
        )}
        <div className="min-w-[520px]">
          <div className="ml-6 grid grid-cols-24 gap-[2px] text-[10px] text-white/35">
            {Array.from({ length: 24 }, (_, h) => (
              <span key={h} className="tnum text-center">
                {h % 6 === 0 ? h : ""}
              </span>
            ))}
          </div>
          {matrix.map((row, day) => (
            <div key={day} className="mt-[2px] flex items-center gap-[2px]">
              <span className="w-6 shrink-0 text-[10px] text-white/40">{WEEKDAYS[day]}</span>
              <div className="grid flex-1 grid-cols-24 gap-[2px]">
                {row.map((v, h) => (
                  <div
                    key={h}
                    title={`周${WEEKDAYS[day]} ${String(h).padStart(2, "0")}:00 · ${formatWatched(v)}`}
                    className="h-4 rounded-[3px]"
                    style={{
                      background:
                        v > 0
                          ? `rgba(127,176,255,${(0.18 + (v / max) * 0.82).toFixed(2)})`
                          : "rgba(255,255,255,0.04)",
                    }}
                  />
                ))}
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
