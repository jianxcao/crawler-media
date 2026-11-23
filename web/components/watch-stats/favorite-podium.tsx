"use client";

import { LockIcon, StarIcon } from "@/components/icons";
import {
  EmptyAction,
  EmptyState,
  Skeleton,
  TitleText,
  formatWatched,
} from "@/components/playback-stats-section";
import { PosterImage } from "@/components/poster-image";
import type { PlaybackStatsTitleRow } from "@/lib/api/playback";
import { imageUrl } from "@/lib/image-proxy";
import { rankChange } from "@/lib/watch-stats-helpers";

const KIND_LABELS: Partial<Record<string, string>> = { movie: "电影", tv: "剧集" };

function TopEntry({
  row,
  rank,
  hero,
  drilled,
  previous,
  ambient,
}: {
  row: PlaybackStatsTitleRow;
  rank: number;
  hero: boolean;
  drilled: boolean;
  previous: PlaybackStatsTitleRow[];
  ambient: boolean;
}) {
  const change = rankChange(row, rank, previous);
  const kind = KIND_LABELS[row.media.kind];
  const facts = [row.media.year, kind].filter(Boolean).join(" · ");
  const watched = formatWatched(row.watched_ms);
  const plays = `${row.plays} 场`;
  const audience = drilled ? null : `${row.members} 人看过`;
  const meta = [[audience, plays].filter(Boolean).join(" · "), watched];
  return (
    <div className="flex min-w-0 items-end">
      <span
        aria-hidden="true"
        className={`relative inline-block shrink-0 select-none text-left font-black leading-[0.78] tracking-[-0.06em] text-transparent ${
          ambient
            ? "[-webkit-text-stroke:2px_rgba(255,255,255,0.62)]"
            : "[-webkit-text-stroke:2px_var(--line-strong)]"
        } ${
          hero ? "-mr-[0.12em] w-[0.6em] text-[128px]" : "-mr-[0.12em] w-[0.6em] text-[96px]"
        }`}
      >
        {rank + 1}
      </span>
      <PosterImage
        src={row.media.poster_url ? imageUrl(row.media.poster_url, "poster-card") : null}
        alt={row.media.title}
        className={`relative z-10 shrink-0 rounded-lg object-cover shadow-[0_18px_40px_rgba(0,0,0,0.6)] ring-1 ring-white/15 ${
          hero ? "h-[180px] w-[120px]" : "h-[120px] w-[80px]"
        }`}
      />
      <div className="relative z-10 ml-3 min-w-0 flex-1 self-center">
        <p className="mb-1 text-[10px] font-bold tracking-[0.2em] text-white/40">
          {["冠军", "亚军", "季军"][rank]}
        </p>
        <TitleText media={row.media} episode={false} large={hero} />
        {facts && <p className="tnum mt-0.5 text-caption text-white/45">{facts}</p>}
        {meta.map((line) => (
          <p key={line} className="tnum mt-1 text-caption leading-5 text-white/60">
            {line}
          </p>
        ))}
        {change && (
          <p className="mt-2">
            <span
              className={`tnum rounded-md px-1.5 py-0.5 text-[11px] font-semibold ${change.tone}`}
            >
              {change.text}
            </span>
          </p>
        )}
      </div>
    </div>
  );
}

/** 本期最受欢迎 TOP 3 领奖台。 */
export function FavoritePodium({
  favorites,
  previous,
  memberId,
  hiddenCount,
  onShowAll,
}: {
  favorites: PlaybackStatsTitleRow[];
  previous: PlaybackStatsTitleRow[];
  memberId: string | null;
  hiddenCount: number;
  onShowAll: () => void;
}) {
  if (favorites.length === 0) {
    return (
      <div className="rounded-2xl border border-white/[0.08] bg-white/[0.02]">
        <EmptyState
          compact
          icon={
            hiddenCount > 0 ? <LockIcon className="size-3.5" /> : <StarIcon className="size-3.5" />
          }
          title={
            hiddenCount > 0 ? "本期最受欢迎的作品都在你的浏览范围外" : "本期还没有可以上榜的作品"
          }
          description={
            hiddenCount > 0
              ? `${hiddenCount} 部作品来自你设为不可见的库`
              : "播放过的条目已被删除，不再进榜"
          }
          actions={
            hiddenCount > 0 ? <EmptyAction onClick={onShowAll}>显示全部</EmptyAction> : undefined
          }
        />
      </div>
    );
  }
  const drilled = memberId != null;
  const [first, ...rest] = favorites;
  const ambient = first.media.poster_url ? imageUrl(first.media.poster_url, "poster-card") : null;
  const columns = ["1.5fr", ...rest.map(() => "1fr")].join(" ");
  return (
    <section
      aria-label={drilled ? "本期看得最多" : "本期最受欢迎"}
      className={`relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] ${
        ambient ? "dark-media-surface" : ""
      }`}
    >
      {ambient && (
        <div aria-hidden="true" className="pointer-events-none absolute inset-0">
          <PosterImage
            src={ambient}
            alt=""
            className="h-full w-full scale-150 object-cover opacity-45 blur-3xl saturate-150"
          />
          <div className="absolute inset-0 bg-[linear-gradient(90deg,rgba(11,13,19,0.88),rgba(11,13,19,0.72)_45%,rgba(11,13,19,0.9))]" />
        </div>
      )}
      <div className="relative p-4 md:p-5">
        <div className="flex items-center gap-2.5">
          <span className="rounded-[3px] bg-[var(--accent)] px-1.5 py-[3px] text-[10px] font-black leading-none tracking-[0.12em] text-[var(--accent-ink)]">
            TOP 3
          </span>
          <p className="text-ui font-semibold text-white/90">
            {drilled ? "本期看得最多" : "本期最受欢迎"}
          </p>
          <p className="text-caption text-white/35">
            {drilled ? "按观看时长" : "按看过的人数，并列看时长"}
          </p>
        </div>
        <div
          className="mt-5 grid items-end gap-x-5 gap-y-6 md:[grid-template-columns:var(--podium-cols)]"
          style={{ "--podium-cols": columns } as React.CSSProperties}
        >
          <TopEntry row={first} rank={0} hero drilled={drilled} previous={previous} ambient={Boolean(ambient)} />
          {rest.map((row, i) => (
            <TopEntry
              key={row.media.media_item_id}
              row={row}
              rank={i + 1}
              hero={false}
              drilled={drilled}
              previous={previous}
              ambient={Boolean(ambient)}
            />
          ))}
        </div>
      </div>
    </section>
  );
}

/** 读取中的统计骨架：与真实布局同形。 */
export function StatsSkeleton() {
  return (
    <div aria-busy="true" aria-label="正在读取观看统计" className="space-y-4">
      <div className="grid grid-cols-4 gap-2.5 max-md:grid-cols-2">
        {Array.from({ length: 4 }, (_, i) => (
          <div
            key={i}
            className="rounded-2xl border border-white/[0.08] bg-white/[0.03] px-4 py-3.5"
          >
            <Skeleton className="h-3 w-14" />
            <Skeleton className="mt-3 h-7 w-24" />
            <Skeleton className="mt-3 h-3 w-28" />
          </div>
        ))}
      </div>
      <div className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-5">
        <Skeleton className="h-4 w-32" />
        <div className="mt-5 flex items-end gap-6 max-md:flex-col max-md:items-stretch">
          <Skeleton className="h-[180px] w-full md:w-[38%]" />
          <Skeleton className="h-[120px] w-full md:w-[26%]" />
          <Skeleton className="h-[120px] w-full md:w-[26%]" />
        </div>
      </div>
      <div className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-3">
        <Skeleton className="h-3 w-20" />
        <Skeleton className="mt-3 h-[220px] w-full" />
      </div>
      <div className="grid grid-cols-2 gap-4 max-md:grid-cols-1">
        {Array.from({ length: 4 }, (_, i) => (
          <Skeleton key={i} className="h-40 w-full rounded-2xl" />
        ))}
      </div>
    </div>
  );
}
