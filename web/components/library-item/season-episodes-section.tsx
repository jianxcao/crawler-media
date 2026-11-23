"use client";

import { useEffect, useMemo, useRef, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { HScroller } from "@/components/h-scroller";
import { CheckIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import {
  getItemEpisodes,
  type EpisodeAvailabilityStatus,
  type LibraryEpisode,
  type LibraryItemFile,
  type LibrarySeasonSummary,
  type SeasonEpisodes,
} from "@/lib/api/libraries";
import { formatVideoResolution } from "@/lib/format";
import { seasonLabel } from "@/lib/library-item-format";

import { imageUrl } from "./item-image-url";

/** 剧集详情页当前选中的分集上下文，供 Hero 与分集区共享同一份数据。 */
export interface SelectedEpisodeContext<F = LibraryItemFile> {
  seasonNumber: number;
  episode: LibraryEpisode;
  files: F[];
}

/** 分集区需要的条目最小形状（详情视图与分享页的访客视图都满足）。 */
export interface EpisodeSectionItem<F extends { file_id: string; season: number | null }> {
  media_item_id: string;
  seasons: number[];
  season_summaries?: LibrarySeasonSummary[];
  files: F[];
}

export function resolveLibraryEpisodeStatus(episode: LibraryEpisode): EpisodeAvailabilityStatus {
  if (episode.status) return episode.status;
  return episode.owned ? "in_library" : "missing";
}

function formatEpisodeResolution(resolution: string | null | undefined): string | null {
  if (!resolution) return null;
  const formatted =
    typeof formatVideoResolution === "function" ? formatVideoResolution(resolution) : resolution;
  return formatted || resolution;
}

function formatSeasonOptionLabel(
  seasonNumber: number,
  owned: boolean,
  summary?: LibrarySeasonSummary,
): string {
  const base = seasonNumber === 0 ? "特别篇" : `第 ${seasonNumber} 季`;
  if (summary && summary.total_count > 0) {
    return `${base} (${summary.owned_count}/${summary.total_count})`;
  }
  return seasonLabel(seasonNumber, owned);
}

/**
 * 剧集分集区（播放器式横滑剧照 + MoviePilot 式全季 5 态网格切换）。
 */
export function SeasonEpisodesSection<F extends { file_id: string; season: number | null }>({
  libraryId,
  detail,
  initialSeason,
  initialEpisode,
  onEpisodeChange,
  fetchEpisodes,
  refreshKey = 0,
}: {
  libraryId: string;
  detail: EpisodeSectionItem<F>;
  initialSeason?: number;
  initialEpisode?: number;
  onEpisodeChange?: (selection: SelectedEpisodeContext<F> | null) => void;
  fetchEpisodes?: (mediaItemId: string, season: number) => Promise<SeasonEpisodes>;
  refreshKey?: number;
}) {
  const summaryBySeason = useMemo(
    () => new Map((detail.season_summaries ?? []).map((s) => [s.season_number, s])),
    [detail.season_summaries],
  );
  const seasons = useMemo(() => {
    if (detail.season_summaries && detail.season_summaries.length > 0) {
      const merged = new Set<number>(detail.seasons);
      for (const s of detail.season_summaries) merged.add(s.season_number);
      return [...merged].sort((a, b) => a - b);
    }
    return detail.seasons;
  }, [detail.seasons, detail.season_summaries]);

  const ownedSeasons = useMemo(() => {
    const set = new Set<number>(detail.seasons);
    for (const s of detail.season_summaries ?? []) {
      if (s.owned_count > 0) set.add(s.season_number);
    }
    return set;
  }, [detail.seasons, detail.season_summaries]);

  const defaultSeason =
    seasons.find((s) => s > 0 && ownedSeasons.has(s)) ??
    seasons.find((s) => ownedSeasons.has(s)) ??
    seasons.find((s) => s > 0) ??
    seasons[0] ??
    1;
  const requestedSeason =
    initialSeason != null && seasons.includes(initialSeason) ? initialSeason : undefined;
  const [season, setSeason] = useState(() => requestedSeason ?? defaultSeason);
  const [viewMode, setViewMode] = useState<"stills" | "grid">("stills");
  const [data, setData] = useState<SeasonEpisodes | null>(null);
  const [failed, setFailed] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const sectionRef = useRef<HTMLElement>(null);
  const initialEpisodeScrolled = useRef(false);

  const silentRefreshRef = useRef<() => Promise<SeasonEpisodes>>(() => Promise.reject());
  silentRefreshRef.current = () =>
    fetchEpisodes
      ? fetchEpisodes(detail.media_item_id, season)
      : getItemEpisodes(libraryId, detail.media_item_id, season);

  useEffect(() => {
    let cancelled = false;
    setData(null);
    setFailed(false);
    (fetchEpisodes
      ? fetchEpisodes(detail.media_item_id, season)
      : getItemEpisodes(libraryId, detail.media_item_id, season)
    )
      .then((result) => {
        if (cancelled) return;
        setData(result);
        const requested =
          season === requestedSeason && initialEpisode != null
            ? (result.episodes.find(
                (episode) => episode.episode_number === initialEpisode && episode.owned,
              ) ?? result.episodes.find((episode) => episode.episode_number === initialEpisode))
            : undefined;
        const first = requested ?? result.episodes.find((e) => e.owned) ?? result.episodes[0];
        setSelected(first?.episode_number ?? null);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [
    libraryId,
    detail.media_item_id,
    detail.files.length,
    season,
    requestedSeason,
    initialEpisode,
    fetchEpisodes,
  ]);

  useEffect(() => {
    if (!refreshKey) return;
    let cancelled = false;
    const requestedForSeason = season;
    silentRefreshRef
      .current()
      .then((result) => {
        if (!cancelled && result.season_number === requestedForSeason) setData(result);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [refreshKey, season]);

  useEffect(() => {
    if (
      initialEpisodeScrolled.current ||
      season !== requestedSeason ||
      selected == null ||
      selected !== initialEpisode
    ) {
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      sectionRef.current
        ?.querySelector<HTMLElement>(`[data-episode-number="${selected}"]`)
        ?.scrollIntoView({ behavior: "smooth", inline: "center", block: "nearest" });
      initialEpisodeScrolled.current = true;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [initialEpisode, requestedSeason, season, selected]);

  const filesById = useMemo(
    () => new Map(detail.files.map((f) => [f.file_id, f])),
    [detail.files],
  );
  const current =
    data !== null && data.season_number === season
      ? (data.episodes.find((e) => e.episode_number === selected) ?? null)
      : null;
  const currentFiles = useMemo(
    () =>
      (current?.file_ids ?? [])
        .map((id) => filesById.get(id))
        .filter((f): f is F => Boolean(f)),
    [current, filesById],
  );
  const counts = useMemo(() => {
    let owned = 0;
    let downloading = 0;
    let missing = 0;
    for (const ep of data?.episodes ?? []) {
      const st = resolveLibraryEpisodeStatus(ep);
      if (ep.owned || st === "in_library") owned += 1;
      else if (st === "downloading") downloading += 1;
      else if (st === "missing" || st === "file_missing") missing += 1;
    }
    return { owned, downloading, missing };
  }, [data]);

  useEffect(() => {
    onEpisodeChange?.(
      current
        ? {
            seasonNumber: season,
            episode: current,
            files: currentFiles,
          }
        : null,
    );
  }, [current, currentFiles, onEpisodeChange, season]);

  return (
    <section ref={sectionRef}>
      <SeasonEpisodesHeader
        seasons={seasons}
        season={season}
        onSeasonChange={setSeason}
        ownedSeasons={ownedSeasons}
        summaryBySeason={summaryBySeason}
        data={data}
        counts={counts}
        viewMode={viewMode}
        onViewModeChange={setViewMode}
      />

      {failed && (
        <p className="text-sub text-[var(--text-muted)]">分集信息加载失败，请稍后重试。</p>
      )}
      {!data && !failed && (
        <div className="flex items-center gap-2.5 py-6 text-sub text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在读取分集信息…
        </div>
      )}

      {data && viewMode === "stills" && (
        <HScroller className="-mx-1 gap-3 px-1 pb-1 pt-1">
          {data.episodes.map((episode) => (
            <EpisodeCard
              key={episode.episode_number}
              episode={episode}
              selected={episode.episode_number === selected}
              onSelect={() => setSelected(episode.episode_number)}
            />
          ))}
        </HScroller>
      )}

      {data && viewMode === "grid" && (
        <div
          data-view="grid"
          className="grid grid-cols-2 gap-2.5 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
        >
          {data.episodes.map((episode) => (
            <EpisodeGridCell
              key={episode.episode_number}
              episode={episode}
              selected={episode.episode_number === selected}
              onSelect={() => setSelected(episode.episode_number)}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function SeasonEpisodesHeader({
  seasons,
  season,
  onSeasonChange,
  ownedSeasons,
  summaryBySeason,
  data,
  counts,
  viewMode,
  onViewModeChange,
}: {
  seasons: number[];
  season: number;
  onSeasonChange: (season: number) => void;
  ownedSeasons: Set<number>;
  summaryBySeason: Map<number, LibrarySeasonSummary>;
  data: SeasonEpisodes | null;
  counts: { owned: number; downloading: number; missing: number };
  viewMode: "stills" | "grid";
  onViewModeChange: (mode: "stills" | "grid") => void;
}) {
  return (
    <div className="mb-3 flex flex-wrap items-center gap-3">
      <h2 className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
        分集
      </h2>
      {seasons.length > 1 ? (
        <select
          value={season}
          onChange={(e) => onSeasonChange(Number(e.target.value))}
          className="tnum rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-1.5 text-sub text-white/90 outline-none focus:border-white/25"
        >
          {seasons.map((s) => (
            <option key={s} value={s}>
              {formatSeasonOptionLabel(s, ownedSeasons.has(s), summaryBySeason.get(s))}
            </option>
          ))}
        </select>
      ) : (
        <span className="text-sub text-[var(--text-muted)]">
          {seasonLabel(season, ownedSeasons.has(season))}
        </span>
      )}
      {data && (
        <div className="tnum flex flex-wrap items-center gap-x-2 gap-y-1 text-sub text-[var(--text-faint)]">
          <span>{`在库 ${counts.owned} / ${data.episodes.length} 集`}</span>
          {counts.downloading > 0 && (
            <span className="text-sky-300/90">{`· 下载中 ${counts.downloading}`}</span>
          )}
          {counts.missing > 0 && (
            <span className="text-[var(--warn)]">{`· 缺 ${counts.missing}`}</span>
          )}
        </div>
      )}
      {data && data.episodes.length > 0 && (
        <div className="ml-auto inline-flex items-center rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5">
          <button
            type="button"
            aria-pressed={viewMode === "stills"}
            onClick={() => onViewModeChange("stills")}
            className={`rounded-lg px-2.5 py-1 text-caption font-medium transition ${
              viewMode === "stills"
                ? "bg-white/15 text-white"
                : "text-white/55 hover:text-white/85"
            }`}
          >
            横滑剧照
          </button>
          <button
            type="button"
            aria-pressed={viewMode === "grid"}
            onClick={() => onViewModeChange("grid")}
            className={`rounded-lg px-2.5 py-1 text-caption font-medium transition ${
              viewMode === "grid"
                ? "bg-white/15 text-white"
                : "text-white/55 hover:text-white/85"
            }`}
          >
            全季网格
          </button>
        </div>
      )}
    </div>
  );
}

function CardStatusBadge({ status }: { status: EpisodeAvailabilityStatus }) {
  if (status === "in_library") return null;
  if (status === "downloading") {
    return (
      <span className="rounded border border-sky-400/40 bg-sky-950/80 px-1.5 py-px text-micro font-semibold text-sky-200 backdrop-blur-sm">
        下载中
      </span>
    );
  }
  if (status === "file_missing") {
    return (
      <span className="rounded border border-orange-400/40 bg-orange-950/80 px-1.5 py-px text-micro font-semibold text-orange-200 backdrop-blur-sm">
        文件缺失
      </span>
    );
  }
  if (status === "unaired") {
    return (
      <span className="rounded bg-black/65 px-1.5 py-px text-micro font-medium text-white/50">
        待播
      </span>
    );
  }
  return (
    <span className="rounded bg-black/65 px-1.5 py-px text-micro font-semibold text-[var(--warn)]">
      缺失
    </span>
  );
}

function EpisodeCard({
  episode,
  selected,
  onSelect,
}: {
  episode: LibraryEpisode;
  selected: boolean;
  onSelect: () => void;
}) {
  const status = resolveLibraryEpisodeStatus(episode);
  const progress = episode.played ? 100 : episode.progress_percent;
  const dimClass =
    status === "in_library"
      ? ""
      : status === "downloading"
        ? "opacity-85"
        : "opacity-45 saturate-50";

  return (
    <button
      type="button"
      data-episode-number={episode.episode_number}
      onClick={onSelect}
      aria-pressed={selected}
      className={`w-[200px] shrink-0 rounded-xl text-left outline-none transition ${dimClass}`}
    >
      <div
        className={`dark-media-surface relative aspect-video overflow-hidden rounded-xl bg-[#141824] transition ${
          selected
            ? "ring-2 ring-white/85 shadow-[0_10px_30px_rgba(0,0,0,0.5)]"
            : "ring-1 ring-white/[0.08] hover:ring-white/35"
        }`}
      >
        <PosterImage
          src={imageUrl(episode.still_url)}
          alt={`第 ${episode.episode_number} 集剧照`}
          className="size-full object-cover"
          fallback={
            <span className="tnum flex size-full items-center justify-center text-[22px] font-bold text-white/20">
              {episode.episode_number}
            </span>
          }
        />
        {(status !== "in_library" || episode.played) && (
          <div className="pointer-events-none absolute right-1.5 top-1.5 flex items-center gap-1">
            <CardStatusBadge status={status} />
            {episode.played && (
              <span
                aria-label="已看完"
                className="flex size-5 items-center justify-center rounded-full bg-[var(--ok)] text-[#07120c] shadow-lg"
              >
                <CheckIcon className="size-3 stroke-[2.5]" />
              </span>
            )}
          </div>
        )}
        {progress != null ? (
          <div className="pointer-events-none absolute inset-x-1.5 bottom-1.5 h-[3px] overflow-hidden rounded-full bg-white/25">
            <div
              className={`h-full rounded-full ${episode.played ? "bg-[var(--ok)]" : "bg-[var(--accent-2)]"}`}
              style={{ width: `${progress}%` }}
            />
          </div>
        ) : (
          episode.position_ms > 0 && (
            <div className="pointer-events-none absolute inset-x-1.5 bottom-1.5 h-[3px] rounded-full bg-[var(--accent-2)]/60" />
          )
        )}
      </div>
      <p
        className={`tnum mt-1.5 truncate text-sub ${
          selected ? "font-semibold text-white" : "text-[var(--text)]"
        }`}
      >
        {episode.episode_number}. {episode.name ?? `第 ${episode.episode_number} 集`}
      </p>
    </button>
  );
}

function GridStatusPill({
  status,
  resolution,
}: {
  status: EpisodeAvailabilityStatus;
  resolution?: string | null;
}) {
  if (status === "in_library") {
    const res = formatEpisodeResolution(resolution);
    return (
      <span className="rounded border border-[var(--ok)]/30 bg-[var(--ok)]/12 px-1.5 py-px text-micro font-semibold text-[var(--ok)]">
        {res ? `在库 · ${res}` : "在库"}
      </span>
    );
  }
  if (status === "downloading") {
    return (
      <span className="rounded border border-[var(--info)]/30 bg-[var(--info)]/12 px-1.5 py-px text-micro font-semibold text-[var(--info)]">
        下载中
      </span>
    );
  }
  if (status === "file_missing") {
    return (
      <span className="rounded border border-[var(--danger)]/30 bg-[var(--danger)]/12 px-1.5 py-px text-micro font-semibold text-[var(--danger)]">
        文件缺失
      </span>
    );
  }
  if (status === "unaired") {
    return (
      <span className="rounded border border-white/10 bg-white/[0.04] px-1.5 py-px text-micro font-medium text-white/45">
        待播
      </span>
    );
  }
  return (
    <span className="rounded border border-[var(--warn)]/30 bg-[var(--warn)]/12 px-1.5 py-px text-micro font-semibold text-[var(--warn)]">
      缺失
    </span>
  );
}

function EpisodeGridCell({
  episode,
  selected,
  onSelect,
}: {
  episode: LibraryEpisode;
  selected: boolean;
  onSelect: () => void;
}) {
  const status = resolveLibraryEpisodeStatus(episode);
  const progress = episode.played ? 100 : episode.progress_percent;
  const surfaceClass = selected
    ? "border-white/70 bg-white/[0.08] shadow-[0_8px_24px_rgba(0,0,0,0.4)]"
    : status === "in_library"
      ? "border-emerald-400/20 bg-emerald-500/[0.04] hover:border-emerald-400/40"
      : status === "downloading"
        ? "border-sky-400/20 bg-sky-500/[0.04] hover:border-sky-400/40"
        : status === "unaired"
          ? "border-white/[0.05] bg-white/[0.015] opacity-60 hover:opacity-85"
          : "border-white/[0.07] bg-white/[0.025] hover:border-white/20";

  return (
    <button
      type="button"
      data-episode-number={episode.episode_number}
      onClick={onSelect}
      aria-pressed={selected}
      className={`relative flex flex-col justify-between overflow-hidden rounded-xl border p-3 text-left outline-none transition ${surfaceClass}`}
    >
      <div className="flex items-center justify-between gap-1.5">
        <span className="tnum text-sub font-bold text-white/90">
          E{String(episode.episode_number).padStart(2, "0")}
        </span>
        <div className="flex items-center gap-1">
          <GridStatusPill status={status} resolution={episode.resolution} />
          {episode.played && (
            <span
              aria-label="已看完"
              className="flex size-4 items-center justify-center rounded-full bg-[var(--ok)] text-[#07120c]"
            >
              <CheckIcon className="size-2.5 stroke-[2.5]" />
            </span>
          )}
        </div>
      </div>
      <p className="mt-1.5 truncate text-caption text-white/75" title={episode.name ?? undefined}>
        {episode.name ?? `第 ${episode.episode_number} 集`}
      </p>
      {episode.air_date && (
        <p className="tnum mt-0.5 text-micro text-white/35">{episode.air_date}</p>
      )}
      {progress != null && (
        <div className="pointer-events-none absolute inset-x-0 bottom-0 h-[2px] bg-white/15">
          <div
            className={`h-full ${episode.played ? "bg-[var(--ok)]" : "bg-[var(--accent-2)]"}`}
            style={{ width: `${progress}%` }}
          />
        </div>
      )}
    </button>
  );
}
