"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";

import { DiscoveryFilterControl } from "@/components/discovery-filter-dialog";
import { FilteredDiscoveryView } from "@/components/filtered-discovery-view";
import { MediaRow } from "@/components/media-row";
import {
  browseDiscoveryCollection,
  collectionHref,
  collectionToRow,
  fetchDiscoveryPage,
  type DiscoveryPageData,
  type DiscoveryPageSection,
} from "@/lib/api/discover";
import {
  discoveryFilterCount,
  discoveryFiltersKey,
  discoveryFiltersQuery,
  EMPTY_DISCOVERY_FILTERS,
  type DiscoveryFilters,
} from "@/lib/discovery-filters";
import {
  discoverySectionMessage,
  discoverySectionStatus,
  heroFailureMessage,
  retryCollectionCacheKeys,
} from "@/lib/discovery-section-state";
import type {
  MediaItem,
  MediaRowData,
  MediaSource,
  MediaType,
} from "@/lib/media-types";
import { usePageChrome } from "@/lib/page-chrome";
import { useTheme } from "@/lib/ui-prefs";
import { useIsMobile } from "@/lib/use-media-query";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

import {
  DiscoverError,
  DiscoverSkeleton,
  HeroSkeleton,
  MediaTypeSwitcher,
  RowSkeleton,
  SourceSwitcher,
  toErrorInfo,
  type DiscoverErrorInfo,
} from "./discover/discover-chrome";
import { HeroBanner } from "./discover/discover-hero-banner";

const pageCache = new Map<string, DiscoveryPageData>();
const collectionCache = new Map<string, MediaRowData>();

/**
 * 发现页（发现电影 / 发现剧集）：Hero 大横幅 + 分类横滚行。
 */
export function DiscoverView({
  mediaType,
  source,
  filters,
  currentYear,
}: {
  mediaType: MediaType;
  source: MediaSource;
  filters: DiscoveryFilters;
  currentYear: number;
}) {
  const navigate = useNavigate();
  const cacheKey = `${mediaType}:${source}`;
  const filterKey = discoveryFiltersKey(filters);
  const filtering = source === "tmdb" && discoveryFilterCount(filters) > 0;
  const scrollRef = useScrollRestoration(`discover:${cacheKey}:${filterKey}`);
  const [page, setPage] = useState<DiscoveryPageData | null>(() => pageCache.get(cacheKey) ?? null);
  const [hero, setHero] = useState<MediaItem[] | undefined>();
  const [rowsByRef, setRowsByRef] = useState<Record<string, MediaRowData | "error">>({});
  const [error, setError] = useState<DiscoverErrorInfo | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  const handleRetry = useCallback(() => {
    const keys = retryCollectionCacheKeys(cacheKey, page?.sections ?? []);
    pageCache.delete(keys.pageKey);
    for (const k of keys.collectionKeys) {
      collectionCache.delete(k);
    }
    setRowsByRef({});
    setHero(undefined);
    setError(null);
    setReloadKey((k) => k + 1);
  }, [cacheKey, page?.sections]);

  useEffect(() => {
    if (filtering) return;
    const controller = new AbortController();
    setError(null);
    setHero(undefined);

    const cacheKeyFor = (section: DiscoveryPageSection) =>
      `${section.collectionRef}:${section.previewLimit}`;

    const seedRows = (sections: DiscoveryPageSection[]) => {
      const seeded: Record<string, MediaRowData | "error"> = {};
      for (const section of sections) {
        if (section.error) {
          seeded[section.collectionRef] = "error";
          continue;
        }
        const cached = collectionCache.get(cacheKeyFor(section));
        if (cached) seeded[section.collectionRef] = cached;
      }
      setRowsByRef(seeded);
      return seeded;
    };

    const loadPage = (pageData: DiscoveryPageData) => {
      const heroSection = pageData.sections.find((section) => section.presentation === "hero");
      const rowSections = pageData.sections.filter((section) => section.presentation !== "hero");
      const seeded = seedRows(rowSections);

      if (heroSection) {
        if (heroSection.error) {
          setHero([]);
        } else {
          const cached = collectionCache.get(cacheKeyFor(heroSection));
          if (cached) {
            setHero(cached.items);
          } else {
            browseDiscoveryCollection(heroSection.collectionRef, heroSection.previewLimit, {
              signal: controller.signal,
            })
              .then((collection) => {
                const row = collectionToRow(collection);
                collectionCache.set(cacheKeyFor(heroSection), row);
                if (!controller.signal.aborted) setHero(row.items);
              })
              .catch(() => {
                if (!controller.signal.aborted) setHero([]);
              });
          }
        }
      } else {
        setHero([]);
      }

      const pending = rowSections.filter((section) => !seeded[section.collectionRef]);
      let failed = 0;
      let firstError: DiscoverErrorInfo | null = null;
      for (const section of pending) {
        if (section.error) {
          setRowsByRef((prev) => ({ ...prev, [section.collectionRef]: "error" }));
          continue;
        }
        browseDiscoveryCollection(section.collectionRef, section.previewLimit, {
          signal: controller.signal,
        })
          .then((collection) => {
            const row = collectionToRow(collection);
            collectionCache.set(cacheKeyFor(section), row);
            if (!controller.signal.aborted) {
              setRowsByRef((prev) => ({ ...prev, [section.collectionRef]: row }));
            }
          })
          .catch((err: unknown) => {
            if (controller.signal.aborted) return;
            firstError ??= toErrorInfo(err);
            failed += 1;
            setRowsByRef((prev) => ({ ...prev, [section.collectionRef]: "error" }));
            if (failed === pending.length && pending.length === rowSections.length) {
              setError(firstError);
            }
          });
      }
    };

    const cachedPage = pageCache.get(cacheKey);
    setPage(cachedPage ?? null);
    if (cachedPage) {
      loadPage(cachedPage);
    } else {
      fetchDiscoveryPage(mediaType, source, { signal: controller.signal })
        .then((data) => {
          if (data.sections.some((s) => !s.error && s.items.length > 0)) {
            pageCache.set(cacheKey, data);
          } else {
            pageCache.delete(cacheKey);
          }
          if (controller.signal.aborted) return;
          if (discoverySectionStatus(data.sections) === "all-failed") {
            setPage(data);
            setError({
              message: "所有发现分区暂时无法加载，请重试",
              code: "discover.section_failed",
            });
            return;
          }
          setPage(data);
          loadPage(data);
        })
        .catch((err: unknown) => {
          if (!controller.signal.aborted) setError(toErrorInfo(err));
        });
    }
    return () => controller.abort();
  }, [cacheKey, filtering, mediaType, reloadKey, source]);

  const applyFilters = useCallback(
    (nextFilters: DiscoveryFilters) => {
      const query = discoveryFiltersQuery(nextFilters);
      navigate(`/discover/${mediaType}${query ? `?${query}` : ""}`);
    },
    [mediaType, navigate],
  );

  const switchSource = useCallback(
    (nextSource: MediaSource) => {
      if (nextSource === source) return;
      navigate(`/discover/${mediaType}?source=${nextSource}`);
    },
    [mediaType, navigate, source],
  );

  const chrome = usePageChrome();
  const isMobile = useIsMobile();
  const isNf = useTheme().id === "netflix";
  const switchMediaType = useCallback(
    (next: MediaType) => {
      if (next === mediaType) return;
      navigate(`/discover/${next}?source=${source}`);
    },
    [mediaType, navigate, source],
  );

  const controls = useMemo(
    () => (
      <div className="flex items-center gap-2">
        {isMobile && <MediaTypeSwitcher value={mediaType} onChange={switchMediaType} />}
        {source === "tmdb" && (
          <DiscoveryFilterControl
            mediaType={mediaType}
            filters={filters}
            currentYear={currentYear}
            onApply={applyFilters}
            compact={isMobile}
          />
        )}
        <SourceSwitcher value={source} onChange={switchSource} compact={isMobile} />
      </div>
    ),
    [applyFilters, currentYear, filters, isMobile, mediaType, source, switchMediaType, switchSource],
  );

  const setTopBarActions = chrome?.setTopBarActions;
  useEffect(() => {
    if (!isMobile || !setTopBarActions) return;
    return setTopBarActions(controls);
  }, [controls, isMobile, setTopBarActions]);

  const toolbar = isMobile ? null : isNf ? (
    <div className="fixed right-[4vw] top-[calc(var(--nf-nav-h)_+_12px)] z-20 flex items-center gap-2">
      {controls}
    </div>
  ) : (
    <div className="sticky top-0 z-20 flex items-center justify-end px-6 pb-3 pt-7">{controls}</div>
  );

  if (filtering) {
    return (
      <div
        ref={scrollRef}
        data-scroll-root
        className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10 max-md:pt-4"
      >
        {toolbar}
        <FilteredDiscoveryView
          key={filterKey}
          mediaType={mediaType}
          filters={filters}
          onClear={() => applyFilters(EMPTY_DISCOVERY_FILTERS)}
        />
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex flex-1 flex-col max-md:pt-4 ${isNf ? "relative" : ""}`}>
        {toolbar}
        <DiscoverError error={error} onRetry={handleRetry} />
      </div>
    );
  }
  if (!page) {
    return (
      <div className={`flex flex-1 flex-col max-md:pt-4 ${isNf ? "relative" : ""}`}>
        {toolbar}
        <DiscoverSkeleton fullBleed={isNf} />
      </div>
    );
  }

  return (
    <div
      ref={scrollRef}
      className={`scroll-thin scroll-safe flex-1 overflow-y-auto pb-10 max-md:pt-4 ${
        isNf ? "relative" : ""
      }`}
    >
      {toolbar}
      {page.tmdbConfigured === false && (
        <div className={isNf ? undefined : "px-6 max-md:px-4"}>
          <div className="mt-3 flex items-center gap-2 rounded-xl border border-[#f5c451]/25 bg-[#f5c451]/8 px-4 py-2.5 text-sub text-[#f5c451]">
            <span aria-hidden="true">⚠️</span>
            <span>
              TMDB API key 未配置,当前展示豆瓣数据。到
              <Link
                to="/settings/metadata"
                className="mx-1 underline decoration-dotted underline-offset-2 hover:text-white"
              >
                设置 → 元数据源
              </Link>
              填写后可显示 TMDB 高分/院线榜单。
            </span>
          </div>
        </div>
      )}
      {(() => {
        const heroSection = page.sections.find((section) => section.presentation === "hero");
        if (heroSection?.error) {
          return (
            <div className={isNf ? "px-[4vw]" : "px-6 max-md:px-4"}>
              <div className="flex items-center justify-between rounded-xl border border-white/5 bg-white/[0.02] p-4">
                <div>
                  <h3 className="text-body font-semibold text-[var(--text)]">
                    {heroSection.title}
                  </h3>
                  <p className="mt-0.5 text-caption text-[var(--text-muted)]">
                    {heroFailureMessage(heroSection)}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={handleRetry}
                  className="btn-glass h-8 rounded-full px-3 text-caption font-semibold"
                >
                  重试
                </button>
              </div>
            </div>
          );
        }
        return null;
      })()}
      {page.sections.some((section) => section.presentation === "hero" && !section.error) &&
        hero === undefined && (
          <div className={isNf ? undefined : "px-6 max-md:px-4"}>
            <HeroSkeleton fullBleed={isNf} />
          </div>
        )}
      {hero && hero.length > 0 && (
        <div className={isNf ? undefined : "px-6 max-md:px-4"}>
          <HeroBanner items={hero} fullBleed={isNf} />
        </div>
      )}
      <div className="mt-8 space-y-8">
        {page.sections
          .filter((section) => section.presentation !== "hero")
          .map((section) => {
            const row = rowsByRef[section.collectionRef];
            if (row === "error") {
              return (
                <div
                  key={section.collectionRef}
                  className={`flex items-center justify-between rounded-xl border border-white/5 bg-white/[0.02] p-4 ${
                    isNf ? "mx-[4vw]" : "mx-6 max-md:px-4"
                  }`}
                >
                  <div>
                    <h3 className="text-body font-semibold text-[var(--text)]">{section.title}</h3>
                    <p className="mt-0.5 text-caption text-[var(--text-muted)]">
                      {discoverySectionMessage(section)}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={handleRetry}
                    className="btn-glass h-8 rounded-full px-3 text-caption font-semibold"
                  >
                    重试
                  </button>
                </div>
              );
            }
            if (row && row.items.length === 0) return null;
            if (!row) {
              return <RowSkeleton key={section.collectionRef} stub={section} fullBleed={isNf} />;
            }
            const moreHref = section.supportsFullListing
              ? collectionHref(section.collectionRef)
              : undefined;
            return (
              <MediaRow
                key={row.id}
                row={moreHref ? { ...row, items: row.items.slice(0, 10) } : row}
                moreHref={moreHref}
                ranked={section.presentation === "ranked-row"}
                insetClassName={isNf ? "px-[4vw]" : undefined}
              />
            );
          })}
      </div>
    </div>
  );
}
