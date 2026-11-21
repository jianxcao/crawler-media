"use client";

import { useEffect, useState } from "react";

import { ArtworkImage } from "@/components/artwork-image";
import { BrandLoader } from "@/components/brand-loader";
import type { MediaEpisodeDetail, MediaSeasonOverview } from "@/lib/api/discover";
import { request } from "@/lib/http";
import { imageUrl } from "@/lib/image-proxy";

/** 在线剧集详情的季与分集预览区。 */
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
    let alive = true;
    setLoading(true);
    request<{ ok: boolean; data: { episodes: MediaEpisodeDetail[] } }>(
      `/media/tv/${mediaId}/season/${activeSeason}`,
    )
      .then((res) => {
        if (alive && res?.ok && Array.isArray(res?.data?.episodes)) {
          setEpisodes(res.data.episodes);
        }
      })
      .catch(() => {
        if (alive) setEpisodes([]);
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [mediaId, activeSeason]);

  const currentSeasonInfo = seasons.find((s) => s.season_number === activeSeason);

  return (
    <section className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-4 border-b border-white/[0.08] pb-3">
        <div className="flex items-center gap-2">
          <span className="text-ui font-semibold text-white/90">季与分集</span>
          {currentSeasonInfo?.episode_count && (
            <span className="text-caption text-white/40">
              共 {currentSeasonInfo.episode_count} 集
            </span>
          )}
        </div>
        {seasons.length > 1 && (
          <div className="flex flex-wrap gap-1.5">
            {seasons.map((s) => (
              <button
                key={s.season_number}
                type="button"
                onClick={() => setActiveSeason(s.season_number)}
                className={`rounded-full px-3 py-1 text-caption font-medium transition ${
                  s.season_number === activeSeason
                    ? "bg-white/20 text-white"
                    : "bg-white/[0.04] text-white/55 hover:bg-white/10 hover:text-white/80"
                }`}
              >
                {s.name || `第 ${s.season_number} 季`}
              </button>
            ))}
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
            <div
              key={ep.episode_number}
              className="group relative flex flex-col overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02] p-2.5 transition hover:border-white/15 hover:bg-white/[0.04]"
            >
              {ep.still_url ? (
                <div className="relative aspect-video w-full overflow-hidden rounded-lg bg-white/[0.04]">
                  <ArtworkImage
                    src={imageUrl(ep.still_url ?? null)}
                    alt={ep.name || `第 ${ep.episode_number} 集`}
                    className="size-full object-cover transition duration-300 group-hover:scale-105"
                    loading="lazy"
                  />
                  <span className="absolute bottom-1.5 left-1.5 rounded bg-black/60 px-1.5 py-0.5 text-micro font-medium text-white/90 backdrop-blur-sm">
                    E{String(ep.episode_number).padStart(2, "0")}
                  </span>
                </div>
              ) : (
                <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-white/[0.03] text-sub font-semibold text-white/20">
                  E{String(ep.episode_number).padStart(2, "0")}
                </div>
              )}
              <div className="mt-2 min-w-0 flex-1">
                <div className="flex items-center justify-between gap-2">
                  <p
                    className="truncate text-sub font-medium text-white/85"
                    title={ep.name || undefined}
                  >
                    第 {ep.episode_number} 集 {ep.name ? `· ${ep.name}` : ""}
                  </p>
                </div>
                {ep.air_date && <p className="mt-0.5 text-micro text-white/40">{ep.air_date}</p>}
                {ep.overview && (
                  <p
                    className="mt-1 line-clamp-2 text-caption leading-relaxed text-white/50"
                    title={ep.overview}
                  >
                    {ep.overview}
                  </p>
                )}
              </div>
            </div>
          ))}
        </div>
      ) : (
        <p className="py-4 text-sub text-white/35">暂无分集详细信息</p>
      )}
    </section>
  );
}
