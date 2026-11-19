"use client";

import type { RoutingOptions } from "@/lib/api/libraries";
import type { LibraryKind } from "@/lib/media-types";

import { LABEL_CLASS } from "./form-helpers";

/** 收藏范围：区域逐国勾选（预设组是一键整组的快捷键）+ 类型多选，两个维度间是"且"。 */
export function ScopeEditor({
  kind,
  regions,
  genres,
  onRegions,
  onGenres,
  options,
}: {
  kind: LibraryKind;
  regions: string[];
  genres: number[];
  onRegions: (next: string[]) => void;
  onGenres: (next: number[]) => void;
  options: RoutingOptions | null;
}) {
  if (options === null) {
    return (
      <p className="rounded-xl border border-white/[0.08] bg-white/[0.03] px-3 py-2.5 text-sub text-[var(--text-faint)]">
        正在加载可选项…
      </p>
    );
  }
  const genreOptions = kind === "movie" ? options.movie_genres : options.tv_genres;
  const allCountries = Object.keys(options.country_names);
  return (
    <>
      <div>
        <label className={LABEL_CLASS}>区域（勾选任一即匹配）</label>
        {/* 已选中但不在内置映射里的码补进列表，保证选了就能看见、能取消 */}
        <div className="flex flex-wrap gap-1.5">
          {[
            ...Object.entries(options.country_names),
            ...regions
              .filter((c) => !(c in options.country_names))
              .map((c): [string, string] => [c, c]),
          ].map(([code, label]) => (
            <button
              key={code}
              type="button"
              data-active={regions.includes(code)}
              onClick={() =>
                onRegions(
                  regions.includes(code) ? regions.filter((c) => c !== code) : [...regions, code],
                )
              }
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              {label}
            </button>
          ))}
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          <span className="text-caption text-[var(--text-faint)]">快捷组合</span>
          <button
            type="button"
            onClick={() =>
              onRegions(
                allCountries.every((c) => regions.includes(c))
                  ? []
                  : [...new Set([...regions, ...allCountries])],
              )
            }
            className="glass-row nav-item !w-auto px-2.5 py-1 text-caption font-medium"
          >
            {allCountries.every((c) => regions.includes(c)) ? "清空" : "全选"}
          </button>
          {options.region_presets.map((preset) => {
            const active = preset.countries.every((c) => regions.includes(c));
            return (
              <button
                key={preset.key}
                type="button"
                data-active={active}
                title={preset.countries.map((c) => options.country_names[c] ?? c).join(" / ")}
                onClick={() =>
                  onRegions(
                    active
                      ? regions.filter((c) => !preset.countries.includes(c))
                      : [...new Set([...regions, ...preset.countries])],
                  )
                }
                className="glass-row nav-item !w-auto px-2.5 py-1 text-caption font-medium"
              >
                {preset.label}
              </button>
            );
          })}
        </div>
      </div>
      <div>
        <label className={LABEL_CLASS}>类型（勾选任一即匹配）</label>
        <div className="flex flex-wrap items-center gap-1.5">
          <button
            type="button"
            onClick={() =>
              onGenres(
                genreOptions.every((g) => genres.includes(g.id))
                  ? []
                  : [...new Set([...genres, ...genreOptions.map((g) => g.id)])],
              )
            }
            className="glass-row nav-item !w-auto px-2.5 py-1 text-caption font-medium"
          >
            {genreOptions.every((g) => genres.includes(g.id)) ? "清空" : "全选"}
          </button>
          {genreOptions.map((g) => (
            <button
              key={g.id}
              type="button"
              data-active={genres.includes(g.id)}
              onClick={() =>
                onGenres(
                  genres.includes(g.id) ? genres.filter((id) => id !== g.id) : [...genres, g.id],
                )
              }
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              {g.label}
            </button>
          ))}
        </div>
      </div>
      {regions.length > 0 && genres.length > 0 && (
        <p className="text-caption leading-relaxed text-[var(--text-faint)]">
          区域与类型须<strong className="font-medium text-[var(--text-muted)]">同时满足</strong>
          （如「日韩 + 动画」= 只收日韩的动画）。
        </p>
      )}
    </>
  );
}
