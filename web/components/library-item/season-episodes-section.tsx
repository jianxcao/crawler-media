"use client";

import { useEffect, useMemo, useRef, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { HScroller } from "@/components/h-scroller";
import { CheckIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import {
  getItemEpisodes,
  type LibraryEpisode,
  type LibraryItemFile,
  type SeasonEpisodes,
} from "@/lib/api/libraries";
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
  files: F[];
}

/**
 * 剧集分集区（播放器式）：季选择器 + 分集横滚缩略图卡。
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
  const seasons = detail.seasons;
  const ownedSeasons = useMemo(() => new Set(detail.seasons), [detail.seasons]);
  const defaultSeason = seasons.find((s) => ownedSeasons.has(s)) ?? seasons[0];
  const requestedSeason =
    initialSeason != null && ownedSeasons.has(initialSeason) ? initialSeason : undefined;
  const [season, setSeason] = useState(() => requestedSeason ?? defaultSeason);
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
            ? result.episodes.find(
                (episode) => episode.episode_number === initialEpisode && episode.owned,
              )
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
  const ownedCount = data ? data.episodes.filter((e) => e.owned).length : 0;

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
      <div className="mb-3 flex items-center gap-3">
        <h2 className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
          分集
        </h2>
        {seasons.length > 1 ? (
          <select
            value={season}
            onChange={(e) => setSeason(Number(e.target.value))}
            className="rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-1.5 text-sub text-white/90 outline-none focus:border-white/25 [&>option]:bg-[#181c28]"
          >
            {seasons.map((s) => (
              <option key={s} value={s}>
                {seasonLabel(s, ownedSeasons.has(s))}
              </option>
            ))}
          </select>
        ) : (
          <span className="text-sub text-[var(--text-muted)]">
            {seasonLabel(season, ownedSeasons.has(season))}
          </span>
        )}
        {data && (
          <span className="tnum text-sub text-[var(--text-faint)]">
            在库 {ownedCount} / {data.episodes.length} 集
          </span>
        )}
      </div>

      {failed && (
        <p className="text-sub text-[var(--text-muted)]">分集信息加载失败，请稍后重试。</p>
      )}
      {!data && !failed && (
        <div className="flex items-center gap-2.5 py-6 text-sub text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在读取分集信息…
        </div>
      )}

      {data && (
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
    </section>
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
  const progress = episode.played ? 100 : episode.progress_percent;
  return (
    <button
      type="button"
      data-episode-number={episode.episode_number}
      onClick={onSelect}
      aria-pressed={selected}
      className={`w-[200px] shrink-0 rounded-xl text-left outline-none transition ${
        episode.owned ? "" : "opacity-45 saturate-50"
      }`}
    >
      <div
        className={`relative aspect-video overflow-hidden rounded-xl bg-[#141824] transition ${
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
        {(!episode.owned || episode.played) && (
          <div className="pointer-events-none absolute right-1.5 top-1.5 flex items-center gap-1">
            {!episode.owned && (
              <span className="rounded bg-black/60 px-1.5 py-px text-micro font-semibold text-[var(--warn)]">
                缺
              </span>
            )}
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
