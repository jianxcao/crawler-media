"use client";

import { useEffect, useRef, useState } from "react";

import { CheckIcon, ChevronLeftIcon, ChevronRightIcon, PlusIcon, StarIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import { useSubscribeEntry } from "@/components/subscribe-entry";
import { createArtworkImage } from "@/lib/artwork-loading";
import { upgradedTmdbOriginalUrl } from "@/lib/image-proxy";
import { useMediaDetail } from "@/lib/media-detail";
import type { MediaItem } from "@/lib/media-types";
import { useTapGuard } from "@/lib/use-tap-guard";

const HERO_INTERVAL = 8000;

function useHeroBackdrop(revealed: boolean, w1280: string | undefined): string | undefined {
  const original = w1280 ? upgradedTmdbOriginalUrl(w1280) : undefined;
  const upgradable = Boolean(original && original !== w1280);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (!revealed || !upgradable || !original) {
      setReady(false);
      return;
    }
    let cancelled = false;
    const preload = new AbortController();
    const { image: img, load: loadImage } = createArtworkImage(preload.signal);
    const settle = () => {
      img
        .decode()
        .catch(() => {})
        .then(() => {
          if (!cancelled) setReady(true);
        });
    };
    img.onload = settle;
    loadImage(original);
    return () => {
      cancelled = true;
      preload.abort();
    };
  }, [revealed, upgradable, original]);
  return revealed ? (ready ? original : w1280) : undefined;
}

function HeroSlide({
  item,
  active,
  preload,
}: {
  item: MediaItem;
  active: boolean;
  preload: boolean;
}) {
  const { open } = useMediaDetail();
  const { canSubscribe, open: openSubscribe, subscriptionOf } = useSubscribeEntry();
  const existingSub = subscriptionOf(item);
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  const push = active && mounted;
  const [revealed, setRevealed] = useState(preload);
  useEffect(() => {
    if (preload) setRevealed(true);
  }, [preload]);
  const backdropSrc = useHeroBackdrop(revealed, item.backdropUrl);
  const tapGuard = useTapGuard(() => open(item));
  const subscribeTapGuard = useTapGuard(() => void openSubscribe(item));

  return (
    <div
      aria-hidden={!active}
      role={active ? "button" : undefined}
      tabIndex={active ? 0 : -1}
      aria-label={`查看《${item.title}》详情`}
      {...tapGuard}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          open(item);
        }
      }}
      className={`absolute inset-0 transition-opacity duration-700 ease-out focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-ring)] ${
        active ? "z-[1] cursor-pointer opacity-100" : "pointer-events-none z-0 opacity-0"
      }`}
    >
      <PosterImage
        src={backdropSrc}
        alt={`${item.title} 剧照`}
        className={`absolute inset-0 size-full object-cover object-top transition-transform duration-[9000ms] ease-linear ${
          push ? "scale-[1.06]" : "scale-100"
        }`}
      />
      <div className="absolute inset-0 bg-gradient-to-r from-[rgba(7,9,14,0.88)] via-[rgba(7,9,14,0.42)] to-transparent" />
      <div className="absolute inset-x-0 bottom-0 h-1/2 bg-gradient-to-t from-[rgba(7,9,14,0.72)] to-transparent max-md:h-3/4 max-md:from-[rgba(7,9,14,0.9)]" />

      <div
        className={`absolute inset-0 flex max-w-xl flex-col justify-end p-7 transition-all delay-150 duration-500 ease-out max-md:p-4 sm:p-9 ${
          active ? "translate-y-0 opacity-100" : "translate-y-3 opacity-0"
        }`}
      >
        <p className="text-caption font-semibold uppercase tracking-[0.22em] text-[var(--accent-2)]">
          今日精选 · {item.type === "movie" ? "电影" : "剧集"}
        </p>
        <h2 className="text-on-image mt-2 text-[34px] font-bold leading-[1.1] tracking-[-0.02em] text-white max-md:mt-1 max-md:text-[23px] sm:text-[40px]">
          {item.title}
        </h2>
        <p className="text-on-image mt-1 truncate text-ui text-white/55 max-md:text-caption">
          {item.originalTitle}
        </p>

        <div className="tnum mt-3 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-sub text-white/80">
          {item.rating > 0 && (
            <span className="flex items-center gap-1 font-semibold text-white">
              <StarIcon className="size-3.5 text-[var(--warn)]" />
              {item.rating.toFixed(1)}
            </span>
          )}
          <span>{item.year}</span>
          {item.genres.length > 0 && <span>{item.genres.join(" / ")}</span>}
          {item.extent && <span>{item.extent}</span>}
          {item.libraryStatus && (
            <span className="flex items-center gap-1.5 text-emerald-300/90">
              <span className="size-1.5 rounded-full bg-[var(--ok)]" />
              在库
            </span>
          )}
          {item.badges.length > 0 && (
            <span className="flex gap-1.5">
              {item.badges.map((b) => (
                <span
                  key={b}
                  className="rounded border border-white/25 px-1.5 py-px text-micro font-semibold tracking-wide text-white/85"
                >
                  {b}
                </span>
              ))}
            </span>
          )}
        </div>

        <p className="text-on-image mt-3 line-clamp-2 max-w-lg text-ui leading-6 text-white/75 max-md:mt-2 max-md:text-sub max-md:leading-5 sm:line-clamp-3">
          {item.overview}
        </p>

        <div className="mt-5 flex flex-wrap items-center gap-3 max-md:mt-3.5 max-md:gap-2">
          {canSubscribe && (
            <button
              type="button"
              aria-label={
                existingSub ? `管理《${item.title}》的订阅` : `订阅影片《${item.title}》`
              }
              onPointerDown={subscribeTapGuard.onPointerDown}
              onPointerUp={subscribeTapGuard.onPointerUp}
              onPointerCancel={subscribeTapGuard.onPointerCancel}
              onClick={(e) => {
                e.stopPropagation();
                subscribeTapGuard.onClick(e);
              }}
              className={
                existingSub
                  ? "flex h-10 items-center gap-2 rounded-full bg-white/[0.18] px-5 text-ui font-semibold text-white/90 backdrop-blur-md transition-colors hover:bg-white/[0.26]"
                  : "btn-accent flex h-10 items-center gap-2 rounded-full px-5 text-ui font-semibold"
              }
            >
              {existingSub ? (
                <>
                  <CheckIcon className="size-4 text-[var(--ok)]" />
                  已订阅
                </>
              ) : (
                <>
                  <PlusIcon className="size-4" />
                  订阅影片
                </>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/** 发现页精选影片自动轮播大横幅。 */
export function HeroBanner({ items, fullBleed = false }: { items: MediaItem[]; fullBleed?: boolean }) {
  const [index, setIndex] = useState(0);
  const touchStart = useRef<{ x: number; y: number } | null>(null);

  const switchSlide = (offset: number) => {
    setIndex((current) => (current + offset + items.length) % items.length);
  };

  useEffect(() => {
    if (items.length <= 1) return;
    const timer = setInterval(() => {
      if (document.hidden) return;
      setIndex((i) => (i + 1) % items.length);
    }, HERO_INTERVAL);
    return () => clearInterval(timer);
  }, [items.length, index]);

  const next = (index + 1) % items.length;
  return (
    <div
      className={`group relative h-[46vh] min-h-[320px] w-full overflow-hidden max-md:h-[38vh] max-md:min-h-[230px] ${
        fullBleed
          ? ""
          : "rounded-2xl shadow-[0_24px_70px_-18px_rgba(0,0,0,0.62)] ring-1 ring-white/10"
      }`}
      onTouchStart={(e) => {
        const t = e.touches[0];
        touchStart.current = { x: t.clientX, y: t.clientY };
      }}
      onTouchEnd={(e) => {
        const start = touchStart.current;
        touchStart.current = null;
        if (!start || items.length <= 1) return;
        const t = e.changedTouches[0];
        const dx = t.clientX - start.x;
        const dy = t.clientY - start.y;
        if (Math.abs(dx) > 48 && Math.abs(dx) > Math.abs(dy) * 1.5) {
          switchSlide(dx < 0 ? 1 : -1);
        }
      }}
    >
      {items.map((item, i) => (
        <HeroSlide
          key={item.id}
          item={item}
          active={i === index}
          preload={i === index || i === next}
        />
      ))}

      {items.length > 1 && (
        <>
          <button
            type="button"
            aria-label="切换到上一部"
            onClick={() => switchSlide(-1)}
            className="absolute left-3 top-1/2 z-10 flex size-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/35 text-white/80 opacity-0 ring-1 ring-white/15 backdrop-blur-sm transition hover:bg-black/55 hover:text-white group-hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/80 [@media(hover:none)]:hidden"
          >
            <ChevronLeftIcon className="size-6" />
          </button>
          <button
            type="button"
            aria-label="切换到下一部"
            onClick={() => switchSlide(1)}
            className="absolute right-3 top-1/2 z-10 flex size-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/35 text-white/80 opacity-0 ring-1 ring-white/15 backdrop-blur-sm transition hover:bg-black/55 hover:text-white group-hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/80 [@media(hover:none)]:hidden"
          >
            <ChevronRightIcon className="size-6" />
          </button>
        </>
      )}

      {items.length > 1 && (
        <div className="absolute bottom-4 right-5 z-10 flex gap-1.5">
          {items.map((item, i) => (
            <button
              key={item.id}
              type="button"
              aria-label={`切换到《${item.title}》`}
              onClick={() => setIndex(i)}
              className={`h-1.5 rounded-full transition-all duration-300 ${
                i === index ? "w-5 bg-white/85" : "w-1.5 bg-white/30 hover:bg-white/55"
              }`}
            />
          ))}
        </div>
      )}
    </div>
  );
}
