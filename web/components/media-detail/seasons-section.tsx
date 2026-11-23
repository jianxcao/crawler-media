"use client";

import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";

import { ArtworkImage } from "@/components/artwork-image";
import { BrandLoader } from "@/components/brand-loader";
import {
  fetchTvSeasonEpisodes,
  type EpisodeAvailabilityStatus,
  type MediaEpisodeDetail,
  type MediaSeasonOverview,
} from "@/lib/api/discover";
import { formatVideoResolution } from "@/lib/format";
import { imageUrl } from "@/lib/image-proxy";

export function resolveDiscoverEpisodeStatus(ep: MediaEpisodeDetail): EpisodeAvailabilityStatus {
  if (ep.status) return ep.status;
  if (ep.owned) return "in_library";
  return "missing";
}

function formatResolutionBadge(resolution: string | null | undefined): string | null {
  if (!resolution) return null;
  const formatted =
    typeof formatVideoResolution === "function" ? formatVideoResolution(resolution) : resolution;
  return formatted || resolution;
}

function DiscoverStatusBadge({
  status,
  resolution,
}: {
  status: EpisodeAvailabilityStatus;
  resolution?: string | null;
}) {
  if (status === "in_library") {
    const res = formatResolutionBadge(resolution);
    return (
      <span className="rounded-md border border-emerald-400/45 bg-emerald-950/80 px-1.5 py-0.5 text-micro font-semibold text-emerald-100 backdrop-blur-sm">
        {res ? `已入库 · ${res}` : "已入库"}
      </span>
    );
  }
  if (status === "downloading") {
    return (
      <span className="rounded-md border border-sky-400/45 bg-sky-950/80 px-1.5 py-0.5 text-micro font-semibold text-sky-100 backdrop-blur-sm">
        下载中
      </span>
    );
  }
  if (status === "file_missing") {
    return (
      <span className="rounded-md border border-orange-400/45 bg-orange-950/80 px-1.5 py-0.5 text-micro font-semibold text-orange-100 backdrop-blur-sm">
        文件缺失
      </span>
    );
  }
  if (status === "unaired") {
    return (
      <span className="rounded-md border border-white/15 bg-black/65 px-1.5 py-0.5 text-micro font-medium text-white/65 backdrop-blur-sm">
        未开播
      </span>
    );
  }
  return (
    <span className="rounded-md border border-amber-400/40 bg-black/65 px-1.5 py-0.5 text-micro font-semibold text-amber-200 backdrop-blur-sm">
      未入库
    </span>
  );
}

function seasonPillLabel(season: MediaSeasonOverview): { label: string; complete: boolean } {
  const base = season.name || (season.season_number === 0 ? "特别篇" : `第 ${season.season_number} 季`);
  const owned = season.owned_episode_count ?? 0;
  const downloading = season.downloading_episode_count ?? 0;
  const total = season.episode_count ?? 0;
  if ((owned > 0 || downloading > 0) && total > 0) {
    return {
      label: `${base} · ${owned}/${total}`,
      complete: owned >= total,
    };
  }
  return { label: base, complete: false };
}

/** 在线剧集详情的季与分集 5 态全景预览区。 */
export function SeasonsSection({
  mediaId,
  seasons,
}: {
  mediaId: string;
  seasons: MediaSeasonOverview[];
}) {
  const [activeSeason, setActiveSeason] = useState<number>(() => {
    return seasons.find((s) => s.season_number > 0)?.season_number ?? seasons[0]?.season_number ?? 1;
  });
  const [episodes, setEpisodes] = useState<MediaEpisodeDetail[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (seasons.length === 0 || seasons.some((s) => s.season_number === activeSeason)) return;
    const next =
      seasons.find((s) => s.season_number > 0)?.season_number ?? seasons[0]?.season_number ?? 1;
    setActiveSeason(next);
  }, [seasons, activeSeason]);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    fetchTvSeasonEpisodes(mediaId, activeSeason, { signal: controller.signal })
      .then((loaded) => {
        if (!controller.signal.aborted) setEpisodes(loaded);
      })
      .catch(() => {
        if (!controller.signal.aborted) setEpisodes([]);
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => {
      controller.abort();
    };
  }, [mediaId, activeSeason]);

  const currentSeasonInfo = seasons.find((s) => s.season_number === activeSeason);
  const summary = useMemo(() => {
    let inLibrary = 0;
    let downloading = 0;
    let fileMissing = 0;
    let missing = 0;
    let unaired = 0;
    for (const ep of episodes) {
      const st = resolveDiscoverEpisodeStatus(ep);
      if (st === "in_library") inLibrary += 1;
      else if (st === "downloading") downloading += 1;
      else if (st === "file_missing") fileMissing += 1;
      else if (st === "unaired") unaired += 1;
      else missing += 1;
    }
    const total = episodes.length || currentSeasonInfo?.episode_count || 0;
    return { total, inLibrary, downloading, fileMissing, missing, unaired };
  }, [episodes, currentSeasonInfo]);

  return (
    <section className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-4 border-b border-white/[0.08] pb-3">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span className="text-ui font-semibold text-white/90">季与分集</span>
          {summary.total > 0 && (
            <span className="tnum text-caption text-white/45">{`共 ${summary.total} 集`}</span>
          )}
          {episodes.length > 0 && summary.inLibrary > 0 && (
            <span className="tnum text-caption text-emerald-300/90">
              {`· 已入库 ${summary.inLibrary}`}
            </span>
          )}
          {episodes.length > 0 && summary.downloading > 0 && (
            <span className="tnum text-caption text-sky-300/90">
              {`· 下载中 ${summary.downloading}`}
            </span>
          )}
          {episodes.length > 0 && summary.fileMissing > 0 && (
            <span className="tnum text-caption text-orange-300/90">
              {`· 文件缺失 ${summary.fileMissing}`}
            </span>
          )}
          {episodes.length > 0 && summary.missing > 0 && (
            <span className="tnum text-caption text-amber-200/75">
              {`· 缺失 ${summary.missing}`}
            </span>
          )}
          {episodes.length > 0 && summary.unaired > 0 && (
            <span className="tnum text-caption text-white/35">
              {`· 未开播 ${summary.unaired}`}
            </span>
          )}
        </div>
        {seasons.length > 1 && (
          <div className="flex flex-wrap gap-1.5">
            {seasons.map((s) => {
              const { label, complete } = seasonPillLabel(s);
              const active = s.season_number === activeSeason;
              const pillClass = complete
                ? active
                  ? "border border-emerald-400/45 bg-emerald-500/25 text-emerald-100"
                  : "border border-emerald-400/25 bg-emerald-500/10 text-emerald-300/90 hover:bg-emerald-500/20"
                : active
                  ? "bg-white/20 text-white"
                  : "bg-white/[0.04] text-white/55 hover:bg-white/10 hover:text-white/80";
              return (
                <button
                  key={s.season_number}
                  type="button"
                  onClick={() => setActiveSeason(s.season_number)}
                  className={`tnum rounded-full px-3 py-1 text-caption font-medium transition ${pillClass}`}
                >
                  {label}
                </button>
              );
            })}
          </div>
        )}
      </div>

      {loading ? (
        <div className="flex items-center gap-2 py-8 text-sub text-white/40">
          <BrandLoader className="size-4" />
          正在加载分集信息…
        </div>
      ) : episodes.length > 0 ? (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4">
          {episodes.map((ep) => (
            <DiscoverEpisodeCard key={ep.episode_number} season={activeSeason} ep={ep} />
          ))}
        </div>
      ) : (
        <p className="py-4 text-sub text-white/35">暂无分集详细信息</p>
      )}
    </section>
  );
}

function DiscoverEpisodeCard({ season, ep }: { season: number; ep: MediaEpisodeDetail }) {
  const status = resolveDiscoverEpisodeStatus(ep);
  const playHref =
    status === "in_library" && ep.library_id && ep.media_item_id
      ? `/library/${encodeURIComponent(ep.library_id)}/item/${encodeURIComponent(ep.media_item_id)}?season=${season}&episode=${ep.episode_number}`
      : null;
  const cardClass = `group relative flex flex-col overflow-hidden rounded-xl border p-2.5 transition ${
    status === "in_library"
      ? "border-emerald-400/20 bg-emerald-500/[0.04] hover:border-emerald-400/45 hover:bg-emerald-500/[0.08]"
      : status === "unaired"
        ? "border-white/[0.05] bg-white/[0.015] opacity-55"
        : "border-white/[0.06] bg-white/[0.02] hover:border-white/15 hover:bg-white/[0.04]"
  }`;

  const content = (
    <>
      <div className="dark-media-surface relative aspect-video w-full overflow-hidden rounded-lg bg-[#141824]">
        {ep.still_url ? (
          <ArtworkImage
            src={imageUrl(ep.still_url)}
            alt={ep.name || `第 ${ep.episode_number} 集`}
            className="size-full object-cover transition duration-300 group-hover:scale-105"
            loading="lazy"
          />
        ) : (
          <div className="flex size-full items-center justify-center text-sub font-semibold text-white/35">
            E{String(ep.episode_number).padStart(2, "0")}
          </div>
        )}
        <span className="absolute bottom-1.5 left-1.5 rounded bg-black/60 px-1.5 py-0.5 text-micro font-medium text-white/90 backdrop-blur-sm">
          E{String(ep.episode_number).padStart(2, "0")}
        </span>
        <div className="pointer-events-none absolute right-1.5 top-1.5">
          <DiscoverStatusBadge status={status} resolution={ep.resolution} />
        </div>
      </div>
      <div className="mt-2 min-w-0 flex-1">
        <div className="flex items-center justify-between gap-2">
          <p className="truncate text-sub font-medium text-white/85" title={ep.name || undefined}>
            第 {ep.episode_number} 集 {ep.name ? `· ${ep.name}` : ""}
          </p>
        </div>
        {ep.air_date && <p className="mt-0.5 text-micro text-white/40">{ep.air_date}</p>}
        {ep.overview && (
          <p
            className="mt-1 line-clamp-3 text-caption leading-relaxed text-white/55"
            title={ep.overview}
          >
            {ep.overview}
          </p>
        )}
      </div>
    </>
  );

  if (playHref) {
    return (
      <Link to={playHref} className={cardClass}>
        {content}
      </Link>
    );
  }
  return <div className={cardClass}>{content}</div>;
}
