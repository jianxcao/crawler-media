"use client";

import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { PhotoIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import {
  DownloadButton,
  GrabButton,
  TorrentActionsSheet,
} from "@/components/search/torrent-actions";
import { TorrentRow } from "@/components/search/torrent-row";
import { useTileWindow } from "@/components/wall";
import { ZoomLightbox, type ZoomLightboxSlide } from "@/components/zoom-lightbox";
import type { TorrentHit } from "@/lib/api/search";
import { cachedImageUrl } from "@/lib/image-proxy";
import { formatRelativeTime } from "@/lib/time";
import {
  parsedTitleOf,
  seasonEpisodeChip,
  torrentIdOf,
  torrentPromoBadges,
  torrentSizeLabel,
} from "@/lib/torrent-search-view";
import { layoutPosterGrid } from "@/lib/wall-window";

/**
 * 图览模式的结果主体：带海报的结果渲染成海报网格，没有海报的仍按普通列表排在其后。
 */
export function PosterResults({ hits }: { hits: TorrentHit[] }) {
  const { withPoster, withoutPoster } = useMemo(() => {
    const withPoster: TorrentHit[] = [];
    const withoutPoster: TorrentHit[] = [];
    for (const h of hits) (h.poster_url ? withPoster : withoutPoster).push(h);
    return { withPoster, withoutPoster };
  }, [hits]);

  return (
    <div className="space-y-5">
      {withPoster.length > 0 && <PosterGrid hits={withPoster} />}
      {withoutPoster.length > 0 && (
        <div>
          {withPoster.length > 0 && (
            <p className="text-on-image mb-2 text-caption text-[rgba(243,245,249,0.7)]">
              以下 {withoutPoster.length} 条结果没有海报，按列表展示
            </p>
          )}
          <ul className="space-y-2">
            {withoutPoster.map((hit) => (
              <TorrentRow key={`${hit.site_id}:${torrentIdOf(hit)}`} hit={hit} />
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

const POSTER_GRID_SPEC = { minColumn: 150, gapX: 12, gapY: 12 } as const;
const PROBE_WIDTH = 150;
const noop = () => {};

function hasSpecLine(hit: TorrentHit): boolean {
  return Boolean(hit.release.resolution || torrentSizeLabel(hit.size_bytes));
}

function PosterGrid({ hits }: { hits: TorrentHit[] }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLUListElement>(null);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const observer = new ResizeObserver((entries) => {
      const next = Math.floor(entries[0]?.contentRect.width ?? el.clientWidth);
      setWidth((current) => (current === next ? current : next));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const { metrics, probes } = usePosterCardMetrics(hits[0]);

  const layout = useMemo(() => {
    if (width === 0 || !metrics) return null;
    const columns = Math.max(
      1,
      Math.floor(
        (width + POSTER_GRID_SPEC.gapX) / (POSTER_GRID_SPEC.minColumn + POSTER_GRID_SPEC.gapX),
      ),
    );
    const cellWidth = (width - POSTER_GRID_SPEC.gapX * (columns - 1)) / columns;
    const base = cellWidth * 1.5 + metrics.short;
    return layoutPosterGrid(hits.length, width, { ...POSTER_GRID_SPEC, cellHeight: base }, (i) =>
      hasSpecLine(hits[i]) ? metrics.tall - metrics.short : 0,
    );
  }, [hits, width, metrics]);

  const placements = useMemo(() => layout?.placements ?? [], [layout]);
  const [from, to] = useTileWindow(gridRef, placements);
  const [openIndex, setOpenIndex] = useState<number | null>(null);
  const onViewerChange = useCallback(
    (index: number, open: boolean) => setOpenIndex(open ? index : null),
    [],
  );
  const visible = useMemo(() => {
    const range: number[] = [];
    for (let i = from; i < to; i += 1) range.push(i);
    if (openIndex !== null && (openIndex < from || openIndex >= to)) range.push(openIndex);
    return range;
  }, [from, to, openIndex]);

  return (
    <div ref={containerRef} className="relative">
      {probes}
      <ul ref={gridRef} className="relative" style={{ height: layout?.height ?? 0 }}>
        {layout &&
          visible.map((i) => {
            const hit = hits[i];
            const placement = placements[i];
            return (
              <TorrentPosterCard
                key={`${hit.site_id}:${torrentIdOf(hit)}`}
                hit={hit}
                index={i}
                onViewerChange={onViewerChange}
                x={placement.x}
                y={placement.y}
                width={placement.width}
              />
            );
          })}
      </ul>
    </div>
  );
}

function usePosterCardMetrics(sample: TorrentHit | undefined) {
  const [metrics, setMetrics] = useState<{ short: number; tall: number } | null>(null);
  const tallRef = useRef<HTMLUListElement>(null);
  const shortRef = useRef<HTMLUListElement>(null);

  useLayoutEffect(() => {
    const tall = tallRef.current;
    const short = shortRef.current;
    if (!tall || !short) return;
    const measure = () => {
      const poster = PROBE_WIDTH * 1.5;
      const next = {
        short: short.getBoundingClientRect().height - poster,
        tall: tall.getBoundingClientRect().height - poster,
      };
      if (next.short <= 0) return;
      setMetrics((current) =>
        current &&
        Math.abs(current.short - next.short) < 0.5 &&
        Math.abs(current.tall - next.tall) < 0.5
          ? current
          : next,
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(tall);
    observer.observe(short);
    return () => observer.disconnect();
  }, [sample]);

  const probes = sample ? (
    <div
      aria-hidden="true"
      className="pointer-events-none invisible absolute left-0 top-0"
      style={{ width: PROBE_WIDTH }}
    >
      <ul ref={tallRef}>
        <TorrentPosterCard
          hit={{
            ...sample,
            poster_url: null,
            size_bytes: sample.size_bytes ?? 1024 ** 3,
            release: { ...sample.release, resolution: sample.release.resolution ?? "2160p" },
          }}
          measuring
        />
      </ul>
      <ul ref={shortRef}>
        <TorrentPosterCard
          hit={{
            ...sample,
            poster_url: null,
            size_bytes: null,
            release: { ...sample.release, resolution: null },
          }}
          measuring
        />
      </ul>
    </div>
  ) : null;

  return { metrics, probes };
}

const TorrentPosterCard = memo(function TorrentPosterCard({
  hit,
  index,
  onViewerChange,
  x,
  y,
  width,
  measuring = false,
}: {
  hit: TorrentHit;
  index?: number;
  onViewerChange?: (index: number, open: boolean) => void;
  x?: number;
  y?: number;
  width?: number;
  measuring?: boolean;
}) {
  const [viewerIndex, setViewerIndex] = useState<number | null>(null);
  const open = viewerIndex !== null;
  useEffect(() => {
    if (index === undefined || !onViewerChange) return;
    onViewerChange(index, open);
  }, [index, onViewerChange, open]);
  const [actionsOpen, setActionsOpen] = useState(false);
  const size = torrentSizeLabel(hit.size_bytes);
  const parsedTitle = parsedTitleOf(hit);
  const seChip = seasonEpisodeChip(hit.release);
  const promoBadges = torrentPromoBadges(hit, "poster");
  const seeders = hit.seeders ?? 0;
  const leechers = hit.leechers ?? 0;
  const posterUrl = hit.poster_url ?? null;

  const slides = useMemo<ZoomLightboxSlide[]>(() => {
    if (!posterUrl) return [];
    return [
      {
        key: `0:${posterUrl}`,
        title: hit.title,
        thumbUrl: cachedImageUrl(posterUrl, "wall-tile"),
        screenUrl: cachedImageUrl(posterUrl, "wall-screen"),
        fullUrl: cachedImageUrl(posterUrl),
      },
    ];
  }, [posterUrl, hit.title]);

  return (
    <li
      className={`group overflow-hidden rounded-xl border border-white/[0.08] bg-[rgba(14,16,22,0.75)] transition-colors hover:border-white/[0.2] ${
        measuring ? "relative" : "absolute"
      }`}
      style={measuring ? undefined : { left: x, top: y, width }}
    >
      <div
        role="button"
        tabIndex={0}
        title={hit.title}
        aria-label={`浏览「${hit.title}」的 ${slides.length} 张图片`}
        onClick={() => slides.length > 0 && setViewerIndex(0)}
        onKeyDown={(e) => {
          if ((e.key === "Enter" || e.key === " ") && slides.length > 0) {
            e.preventDefault();
            setViewerIndex(0);
          }
        }}
        className="relative aspect-[2/3] cursor-zoom-in outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-ring)]"
      >
        <PosterImage
          src={posterUrl ? cachedImageUrl(posterUrl, "gallery-tile") : undefined}
          preload
          alt={hit.title}
          className="absolute inset-0 size-full transition-transform duration-500 ease-out group-hover:scale-[1.04]"
          fallback={
            <div className="absolute inset-0 flex items-center justify-center bg-gradient-to-b from-white/[0.05] to-black/40">
              <span className="px-3 text-center text-caption text-[var(--text-faint)]">
                {hit.site_name}
              </span>
            </div>
          }
        />
        <div className="absolute inset-x-1.5 top-1.5 flex items-start justify-between gap-1">
          <span className="flex flex-col items-start gap-1">
            {promoBadges.map((b) => (
              <span
                key={b.text}
                className={`rounded-md px-1.5 py-0.5 text-micro font-semibold ${b.cls}`}
              >
                {b.text}
              </span>
            ))}
          </span>
          <span
            title={`共 ${slides.length} 张图片，点击卡片浏览`}
            className="tnum flex shrink-0 items-center gap-1 rounded-md bg-black/70 px-1.5 py-0.5 text-micro font-medium text-white/85"
          >
            <PhotoIcon className="size-3" />
            {slides.length}
          </span>
        </div>
        {seChip && (
          <span
            className={`absolute bottom-1.5 left-1.5 rounded-md px-1.5 py-0.5 text-micro font-semibold ${
              seChip.pack
                ? "bg-gradient-to-r from-[#4f8cff]/90 to-[#9d6bff]/90 text-white"
                : "bg-black/70 text-[#b9d4ff]"
            }`}
          >
            {seChip.text}
          </span>
        )}
        {(hit.detail_url || hit.enclosure) && (
          <div className="absolute inset-0 flex items-center justify-center gap-2 bg-black/0 opacity-0 transition duration-200 group-hover:bg-black/35 group-hover:opacity-100 max-md:hidden [@media(hover:none)]:hidden">
            {hit.detail_url && (
              <a
                href={hit.detail_url}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => e.stopPropagation()}
                className="rounded-lg border border-white/40 bg-black/40 px-2.5 py-1 text-caption text-white backdrop-blur-sm transition-colors hover:bg-black/60"
              >
                详情
              </a>
            )}
            <GrabButton
              hit={hit}
              className="rounded-lg border border-[#6aa7ff]/50 bg-[#6aa7ff]/25 px-2.5 py-1 text-caption font-medium text-white backdrop-blur-sm transition-colors hover:bg-[#6aa7ff]/40"
            />
            <DownloadButton
              hit={hit}
              className="btn-accent rounded-lg px-2.5 py-1 text-caption font-medium"
            />
          </div>
        )}
      </div>

      <div title={hit.title} className="relative px-2.5 pb-2.5 pt-2">
        {(hit.detail_url || hit.enclosure) && (
          <button
            type="button"
            aria-label={`打开「${hit.title}」的资源操作`}
            onClick={() => setActionsOpen(true)}
            className="absolute inset-0 z-10 hidden cursor-pointer max-md:block [@media(hover:none)]:block"
          />
        )}
        <p
          className={`truncate text-sub font-medium ${
            parsedTitle ? "text-[var(--text)]" : "text-[var(--text-muted)]"
          }`}
        >
          {parsedTitle ?? hit.title}
          {parsedTitle && hit.release.year != null && (
            <span className="tnum ml-1.5 text-caption font-normal text-[var(--text-muted)]">
              {hit.release.year}
            </span>
          )}
        </p>
        {(hit.release.resolution || size) && (
          <p className="tnum mt-0.5 truncate text-caption text-[var(--text-muted)]">
            {[hit.release.resolution, size].filter(Boolean).join(" · ")}
          </p>
        )}
        <p className="mt-0.5 flex items-baseline gap-1.5 text-micro text-[var(--text-faint)]">
          <span className="min-w-0 truncate">
            {hit.site_name}
            {hit.upload_time ? ` · ${formatRelativeTime(hit.upload_time)}` : ""}
          </span>
          <span className="tnum ml-auto shrink-0">
            <span className="text-[#a7d9b6]">↑{seeders}</span>
            <span className="ml-1 text-[#e0c19f]">↓{leechers}</span>
          </span>
        </p>
      </div>

      {viewerIndex !== null && slides.length > 0 && (
        <ZoomLightbox
          label={`浏览图片：${hit.title}`}
          slides={slides}
          index={viewerIndex}
          hasMore={false}
          onIndexChange={setViewerIndex}
          onReachEnd={noop}
          onClose={() => setViewerIndex(null)}
          brokenHint="图床可能已失效或拒绝外链访问"
          actions={
            <>
              {hit.detail_url && (
                <a
                  href={hit.detail_url}
                  target="_blank"
                  rel="noreferrer"
                  className="flex h-8 items-center rounded-full bg-white/[0.12] px-3.5 text-sub font-medium text-white/85 transition-colors hover:bg-white/[0.2] hover:text-white"
                >
                  详情
                </a>
              )}
              <GrabButton
                hit={hit}
                className="flex h-8 items-center rounded-full border border-[#6aa7ff]/50 bg-[#6aa7ff]/25 px-3.5 text-sub font-medium text-white transition-colors hover:bg-[#6aa7ff]/40"
              />
              <DownloadButton
                hit={hit}
                dialogTopmost
                className="btn-accent flex h-8 items-center rounded-full px-3.5 text-sub font-medium"
              />
            </>
          }
        />
      )}
      <TorrentActionsSheet
        hit={hit}
        open={actionsOpen}
        onClose={() => setActionsOpen(false)}
        onViewImages={
          slides.length > 0
            ? () => {
                setActionsOpen(false);
                setViewerIndex(0);
              }
            : undefined
        }
      />
    </li>
  );
});
