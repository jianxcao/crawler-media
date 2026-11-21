"use client";

import { useEffect, useRef, type ReactNode, type RefObject } from "react";

import { ArtworkImage } from "@/components/artwork-image";
import { BrandLoader } from "@/components/brand-loader";
import type { PageNavFallback } from "@/components/page-nav";
import { useIsMobile } from "@/lib/use-media-query";

import { MOBILE_HERO_HEIGHT, computeHeroRecedeProgress } from "../../lib/media-detail-helpers";
import { DetailPageNav, useIsNetflixDesktop } from "../detail-page-nav";

/**
 * Netflix 桌面端详情页滚动退场：下滚时往根节点写入 --nf-hero-recede（0→1）并挂上
 * html.nf-hero-live，使沉浸覆盖层/轮播层渐暗并模糊，滚动过程零 React 重渲染。
 */
export function useDetailHeroRecede(
  scrollRef: RefObject<HTMLDivElement | null> | null,
  active: boolean,
) {
  useEffect(() => {
    if (!active || typeof document === "undefined") return;
    const root = document.documentElement;
    root.classList.add("nf-hero-live");
    const el = scrollRef?.current ?? null;
    if (!el) {
      return () => {
        root.classList.remove("nf-hero-live");
        root.style.removeProperty("--nf-hero-recede");
      };
    }

    let frame = 0;
    const sync = () => {
      frame = 0;
      root.style.setProperty(
        "--nf-hero-recede",
        computeHeroRecedeProgress(el.scrollTop, el.clientHeight),
      );
    };
    const onScroll = () => {
      if (!frame) frame = window.requestAnimationFrame(sync);
    };
    sync();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      if (frame) window.cancelAnimationFrame(frame);
      root.classList.remove("nf-hero-live");
      root.style.removeProperty("--nf-hero-recede");
    };
  }, [active, scrollRef]);
}

/** 详情页加载态 / 失败兜底骨架，统一导航栏占位与退场钩子。 */
export function MediaDetailFallback({
  failed,
  navFallback,
  onBack,
  loadingLabel,
  failedTitle,
  failedDescription,
  failedAction,
  outerAmbientFallback = false,
}: {
  failed: boolean;
  navFallback: PageNavFallback;
  onBack: () => void;
  loadingLabel: string;
  failedTitle: string;
  failedDescription: string;
  failedAction: ReactNode;
  outerAmbientFallback?: boolean;
}) {
  const isNfDesktop = useIsNetflixDesktop();
  useDetailHeroRecede(null, isNfDesktop);

  return (
    <div className={`${outerAmbientFallback ? "ambient-fallback " : ""}flex h-full flex-col`}>
      <DetailPageNav title="" fallback={navFallback} onBack={onBack} />
      <div
        className={`${outerAmbientFallback ? "" : "ambient-fallback "}flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center`}
      >
        {failed ? (
          <>
            <p className="text-body-lg font-semibold text-[var(--text)]">{failedTitle}</p>
            <p className="max-w-sm text-ui leading-6 text-[var(--text-muted)]">
              {failedDescription}
            </p>
            {failedAction}
          </>
        ) : (
          <div className="flex items-center gap-2.5 text-ui text-[var(--text-muted)]">
            <BrandLoader className="size-5" />
            {loadingLabel}
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * 影片详情页统一骨架（MediaDetailView / LibraryItemDetailView 共用）：
 * 承担滚动画布、子页顶栏切换、手机 Hero 图层、桌面氛围留白以及 .detail-content / .detail-lead 钩子。
 */
export function MediaDetailScaffold({
  title,
  navFallback,
  onBack,
  actions,
  mobileHeroSrc,
  netflixBackdrop,
  lead,
  children,
  footerModals,
}: {
  title: string;
  navFallback: PageNavFallback;
  onBack: () => void;
  actions?: ReactNode;
  mobileHeroSrc?: string;
  netflixBackdrop?: ReactNode;
  lead?: ReactNode;
  children: ReactNode;
  footerModals?: ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const isMobile = useIsMobile();
  const isNfDesktop = useIsNetflixDesktop();
  const showMobileHero = isMobile && Boolean(mobileHeroSrc);

  useDetailHeroRecede(scrollRef, isNfDesktop);

  return (
    <div
      ref={scrollRef}
      className={`detail-ambient scroll-thin scroll-safe relative isolate h-full overflow-y-auto rounded-2xl max-md:rounded-none ${
        showMobileHero ? "detail-ambient--hero" : ""
      }`}
    >
      <DetailPageNav title={title} fallback={navFallback} onBack={onBack} actions={actions} />
      {isNfDesktop && netflixBackdrop}

      {showMobileHero && mobileHeroSrc && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-x-0 top-0 z-0 overflow-hidden"
          style={{ height: MOBILE_HERO_HEIGHT }}
        >
          <ArtworkImage
            src={mobileHeroSrc}
            alt=""
            decoding="async"
            className="size-full object-cover object-center"
          />
          <div className="absolute inset-x-0 top-0 h-28 bg-gradient-to-b from-black/45 to-transparent" />
          <div className="absolute inset-x-0 bottom-0 h-[55%] bg-gradient-to-b from-transparent via-black/55 to-black" />
        </div>
      )}

      <div
        className={
          showMobileHero ? undefined : "h-[var(--detail-hero-h)] min-h-[var(--detail-hero-min-h)]"
        }
        style={
          showMobileHero
            ? { height: `max(0px, calc(${MOBILE_HERO_HEIGHT} - 52px - var(--safe-top) - 150px))` }
            : undefined
        }
      />

      <div className="detail-content relative z-10 -mt-28 pb-12 pt-28">
        {lead ? <div className="detail-lead">{lead}</div> : null}
        {children}
      </div>

      {footerModals}
    </div>
  );
}
