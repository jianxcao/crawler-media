"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";

import {
  EmptyHint,
  SearchProgressBar,
  type SiteProgress,
  SiteStatusSummary,
  SkeletonList,
  SnapshotBadge,
} from "@/components/search/site-status-summary";
import {
  DownloadTargetPrefContext,
  GrabContext,
  useDownloadTargetPrefs,
} from "@/components/search/torrent-contexts";
import { AppliedChips, FilterToolbar } from "@/components/search/torrent-filter-toolbar";
import { GroupedResults } from "@/components/search/torrent-grouped-results";
import { PosterResults } from "@/components/search/torrent-poster-grid";
import { TorrentRow } from "@/components/search/torrent-row";
import {
  getTorrentSearchHistoryResults,
  streamSearchTorrents,
  type SiteSearchStatus,
  type TorrentHit,
} from "@/lib/api/search";
import { getSubscription } from "@/lib/api/subscriptions";
import type { SearchScope } from "@/lib/categories";
import { usePermissions } from "@/lib/permissions";
import type { SearchQuery } from "@/lib/search-url";
import {
  aggregateFacets,
  collectEntities,
  computeSmartSortKeys,
  emptyFilters,
  type Filters,
  hasActiveFilters,
  initialView,
  matchesFilters,
  readSortFromUrl,
  type ResultView,
  sortHits,
  type SortState,
  torrentIdOf,
} from "@/lib/torrent-search-view";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

/**
 * 搜索结果页（主内容区）——消费 SSE 流式搜索，结果渐进渲染。
 *
 * 数据流：streamSearchTorrents 逐事件回调，快的站点先出结果先上屏，不被最慢的
 * 站点拖住；筛选与排序是纯前端操作，委派给 lib/torrent-search-view.ts。
 */
export interface SearchResultsProps {
  query: SearchQuery;
  /** 发起实时搜索（快照提示条的「重新搜索」按钮）；不传则不渲染该按钮。 */
  onResearch?: (keyword: string, scope: SearchScope) => void;
  /** 手动选种模式（URL 的 for_sub 参数）：每条资源多一颗「投给订阅」按钮，
   *  点击直接投给该订阅（跳过规则组过滤，身份匹配照常）。 */
  grabForSubscriptionId?: string | null;
}

type Phase = "connecting" | "streaming" | "done" | "error";

function toSiteProgress(s: SiteSearchStatus): SiteProgress {
  return {
    site_id: s.site_id,
    site_name: s.site_name,
    phase: s.error ? "error" : "ok",
    count: s.count,
    error: s.error,
    elapsed_ms: s.elapsed_ms ?? null,
  };
}

function patchSiteProgress(
  list: SiteProgress[],
  siteId: string,
  patch: Partial<SiteProgress>,
): SiteProgress[] {
  return list.map((s) => (s.site_id === siteId ? { ...s, ...patch } : s));
}

export function SearchResults({ query, onResearch, grabForSubscriptionId }: SearchResultsProps) {
  const { canDirectDownload: pageCanDirectDownload } = usePermissions();
  const scrollRef = useScrollRestoration(
    `search:torrent:${query.keyword}:${query.scope.label ?? "all"}:${query.scope.categories.join(",")}:${query.scope.siteIds.join(",")}:${query.snapshotId ?? "live"}`,
  );
  const [phase, setPhase] = useState<Phase>("connecting");
  const [grabTarget, setGrabTarget] = useState<{ id: string; title: string } | null>(null);
  useEffect(() => {
    if (!grabForSubscriptionId) {
      setGrabTarget(null);
      return;
    }
    let cancelled = false;
    getSubscription(grabForSubscriptionId)
      .then((d) => {
        if (!cancelled) setGrabTarget({ id: d.id, title: d.media.title });
      })
      .catch(() => {
        if (!cancelled) setGrabTarget(null);
      });
    return () => {
      cancelled = true;
    };
  }, [grabForSubscriptionId]);

  const [fatalError, setFatalError] = useState<string | null>(null);
  const [items, setItems] = useState<TorrentHit[]>([]);
  const [siteProgress, setSiteProgress] = useState<SiteProgress[]>([]);
  const [totalElapsedMs, setTotalElapsedMs] = useState<number | null>(null);
  const [snapshotAt, setSnapshotAt] = useState<string | null>(null);
  const [filters, setFilters] = useState<Filters>(emptyFilters);
  const [sort, setSort] = useState<SortState>(readSortFromUrl);
  const [view, setView] = useState<ResultView>(initialView);
  const [page, setPage] = useState(1);
  const [loadingMore, setLoadingMore] = useState(false);
  const [exhausted, setExhausted] = useState(false);
  const auxAbortsRef = useRef<Set<AbortController>>(new Set());

  // 视图态（排序 / 展示视图）实时同步进地址栏，不触发路由切换
  useEffect(() => {
    if (typeof window === "undefined") return;
    const params = new URLSearchParams(window.location.search);
    if (sort.key === "seeders" && sort.dir === "desc") params.delete("sort");
    else params.set("sort", `${sort.key}:${sort.dir}`);
    if (view === "poster") params.set("poster", "1");
    else params.delete("poster");
    if (view === "list") params.set("view", "list");
    else params.delete("view");
    const qs = params.toString();
    window.history.replaceState(
      window.history.state,
      "",
      qs ? `${window.location.pathname}?${qs}` : window.location.pathname,
    );
  }, [sort, view]);

  // 查询（关键词/范围/快照）变化即重置状态并发起快照加载或流式搜索
  useEffect(() => {
    const controller = new AbortController();
    setPhase("connecting");
    setFatalError(null);
    setItems([]);
    setSiteProgress([]);
    setTotalElapsedMs(null);
    setSnapshotAt(null);
    setFilters(emptyFilters());
    setPage(1);
    setLoadingMore(false);
    setExhausted(false);
    for (const aux of auxAbortsRef.current) aux.abort();
    auxAbortsRef.current.clear();
    setView((prev) =>
      query.scope.posterMode ? "poster" : prev === "poster" ? "group" : prev,
    );

    if (query.snapshotId != null) {
      getTorrentSearchHistoryResults(query.snapshotId, { signal: controller.signal })
        .then((snap) => {
          setItems(snap.items);
          setSiteProgress(snap.sites.map(toSiteProgress));
          setTotalElapsedMs(snap.elapsed_ms ?? null);
          setSnapshotAt(snap.snapshot_at);
          setPhase("done");
        })
        .catch((err: unknown) => {
          if (controller.signal.aborted) return;
          setPhase("error");
          setFatalError(
            err instanceof Error ? err.message : "快照加载失败，请稍后重试",
          );
        });
      return () => controller.abort();
    }

    const patchSite = (siteId: string, patch: Partial<SiteProgress>) =>
      setSiteProgress((prev) => patchSiteProgress(prev, siteId, patch));

    streamSearchTorrents(
      { keyword: query.keyword, scope: query.scope, saveHistory: true },
      (event) => {
        switch (event.type) {
          case "start":
            setPhase("streaming");
            setSiteProgress(
              event.data.sites.map((s) => ({
                ...s,
                phase: "searching",
                count: 0,
                error: null,
                elapsed_ms: null,
              })),
            );
            break;
          case "site_start":
            break;
          case "site_result": {
            const d = event.data;
            setItems((prev) => [...prev, ...d.items]);
            patchSite(d.site_id, { phase: "ok", count: d.count, elapsed_ms: d.elapsed_ms });
            break;
          }
          case "site_error": {
            const d = event.data;
            patchSite(d.site_id, { phase: "error", error: d.error, elapsed_ms: d.elapsed_ms });
            break;
          }
          case "done":
            setTotalElapsedMs(event.data.elapsed_ms);
            setPhase("done");
            break;
        }
      },
      { signal: controller.signal },
    ).catch((err: unknown) => {
      if (controller.signal.aborted) return;
      setPhase("error");
      setFatalError(err instanceof Error ? err.message : "搜索失败，请稍后重试");
    });
    return () => controller.abort();
  }, [query]);

  const retrySite = (siteId: string) => {
    setItems((prev) => prev.filter((h) => h.site_id !== siteId));
    setSiteProgress((prev) =>
      patchSiteProgress(prev, siteId, {
        phase: "searching",
        count: 0,
        error: null,
        elapsed_ms: null,
      }),
    );
    const controller = new AbortController();
    auxAbortsRef.current.add(controller);
    streamSearchTorrents(
      { keyword: query.keyword, scope: { ...query.scope, siteIds: [siteId] } },
      (event) => {
        if (event.type === "site_result") {
          const d = event.data;
          setItems((prev) => [...prev, ...d.items]);
          setSiteProgress((prev) =>
            patchSiteProgress(prev, d.site_id, {
              phase: "ok",
              count: d.count,
              error: null,
              elapsed_ms: d.elapsed_ms,
            }),
          );
        } else if (event.type === "site_error") {
          const d = event.data;
          setSiteProgress((prev) =>
            patchSiteProgress(prev, d.site_id, {
              phase: "error",
              error: d.error,
              elapsed_ms: d.elapsed_ms,
            }),
          );
        }
      },
      { signal: controller.signal },
    )
      .catch(() => {
        if (controller.signal.aborted) return;
        setSiteProgress((prev) =>
          prev.map((s) =>
            s.site_id === siteId && s.phase === "searching"
              ? { ...s, phase: "error", error: "重试失败，请稍后再试" }
              : s,
          ),
        );
      })
      .finally(() => auxAbortsRef.current.delete(controller));
  };

  const loadMore = () => {
    if (loadingMore || exhausted) return;
    const next = page + 1;
    setLoadingMore(true);
    const controller = new AbortController();
    auxAbortsRef.current.add(controller);
    let added = 0;
    streamSearchTorrents(
      { keyword: query.keyword, scope: query.scope, page: next },
      (event) => {
        if (event.type !== "site_result") return;
        const d = event.data;
        setItems((prev) => {
          const seen = new Set(prev.map((h) => `${h.site_id}/${torrentIdOf(h)}`));
          const fresh = d.items.filter((h) => !seen.has(`${h.site_id}/${torrentIdOf(h)}`));
          added += fresh.length;
          return fresh.length > 0 ? [...prev, ...fresh] : prev;
        });
      },
      { signal: controller.signal },
    )
      .then(() => {
        if (controller.signal.aborted) return;
        setPage(next);
        if (added === 0) setExhausted(true);
      })
      .catch(() => undefined)
      .finally(() => {
        auxAbortsRef.current.delete(controller);
        setLoadingMore(false);
      });
  };

  const facets = useMemo(() => aggregateFacets(items, filters), [items, filters]);
  const entities = useMemo(() => collectEntities(items), [items]);
  const filtered = useMemo(
    () => items.filter((hit) => matchesFilters(hit, filters)),
    [items, filters],
  );
  const sorted = useMemo(() => sortHits(filtered, sort), [filtered, sort]);
  const smartSortKeys = useMemo(() => computeSmartSortKeys(items), [items]);
  const filtering = hasActiveFilters(filters);

  const settledStatuses: SiteSearchStatus[] = useMemo(
    () =>
      siteProgress
        .filter((s) => s.phase !== "searching")
        .map((s) => ({
          site_id: s.site_id,
          site_name: s.site_name,
          count: s.count,
          error: s.error,
          elapsed_ms: s.elapsed_ms,
        })),
    [siteProgress],
  );
  const settledCount = settledStatuses.length;
  const streaming = phase === "connecting" || phase === "streaming";
  const downloadTargetPrefs = useDownloadTargetPrefs(pageCanDirectDownload);

  return (
    <GrabContext.Provider value={grabTarget}>
      <DownloadTargetPrefContext.Provider value={downloadTargetPrefs}>
        <div className="relative flex h-full flex-col">
          {grabTarget && (
            <div className="mx-6 mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl border border-[#6aa7ff]/30 bg-[#6aa7ff]/12 px-4 py-2.5 text-sub text-[#b9d4ff] backdrop-blur-sm max-md:mx-4">
              <span className="min-w-0">
                正在为《{grabTarget.title}》手动选种——点资源上的「投给订阅」直接下载并计入该订阅
                （跳过规则组限制）
              </span>
              <Link
                to={`/subscriptions/${grabTarget.id}`}
                className="ml-auto shrink-0 font-medium text-white/85 hover:underline"
              >
                返回订阅 ›
              </Link>
            </div>
          )}
          <header className="relative z-20 shrink-0 px-6 pb-3 pt-4 max-md:px-4 max-md:pt-3">
            <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
              <h1 className="text-on-image text-title-lg font-semibold tracking-[-0.01em] text-white">
                {query.keyword ? `“${query.keyword}”` : "最新资源"}
              </h1>
              {query.scope.label && (
                <span className="rounded-full bg-black/30 px-2.5 py-0.5 text-caption text-[var(--accent)] backdrop-blur-sm">
                  {query.scope.label}
                </span>
              )}
              {phase === "streaming" && (
                <span className="text-on-image flex items-center gap-1.5 text-sub text-[rgba(243,245,249,0.75)]">
                  <span className="size-1.5 animate-pulse rounded-full bg-[var(--accent)]" />
                  已找到 {items.length} 条
                </span>
              )}
              {phase === "done" && (
                <span className="text-on-image text-sub text-[rgba(243,245,249,0.75)]">
                  {filtering
                    ? `筛选后 ${filtered.length} / 共 ${items.length} 条`
                    : `共 ${items.length} 条结果`}
                </span>
              )}

              <div className="ml-auto flex items-center gap-2">
                {snapshotAt && phase === "done" && (
                  <SnapshotBadge
                    snapshotAt={snapshotAt}
                    onResearch={onResearch ? () => onResearch(query.keyword, query.scope) : undefined}
                  />
                )}
                <SiteStatusSummary
                  sites={siteProgress}
                  streaming={streaming}
                  onRetrySite={snapshotAt ? undefined : retrySite}
                  totalElapsedMs={totalElapsedMs}
                />
              </div>
            </div>
            {phase === "done" && siteProgress.length === 0 && !snapshotAt && (
              <p className="text-on-image mt-2 text-sub text-[rgba(243,245,249,0.7)]">
                当前没有「已启用且验证通过」的站点，请先在设置里配置站点。
              </p>
            )}
            {siteProgress.length > 0 && (
              <>
                <FilterToolbar
                  sites={settledStatuses}
                  facets={facets}
                  filters={filters}
                  onChange={setFilters}
                  sort={sort}
                  smartSortKeys={smartSortKeys}
                  onSortChange={setSort}
                  resultCount={filtered.length}
                  view={view}
                  onViewChange={setView}
                />
                <AppliedChips
                  sites={settledStatuses}
                  filters={filters}
                  onChange={setFilters}
                />
              </>
            )}
            {streaming && (
              <SearchProgressBar settled={settledCount} total={siteProgress.length} />
            )}
          </header>

          <div
            ref={scrollRef}
            className="scroll-thin scroll-safe relative z-0 min-h-0 flex-1 overflow-y-auto px-6 pb-6 max-md:px-4"
          >
            {streaming && items.length === 0 && (
              <SkeletonList siteCount={siteProgress.length} />
            )}
            {phase === "error" && (
              <EmptyHint title="搜索出错" hint={fatalError ?? "搜索失败，请稍后重试"} />
            )}
            {sorted.length > 0 ? (
              view === "poster" ? (
                <PosterResults hits={sorted} />
              ) : view === "group" && entities.size >= 2 ? (
                <GroupedResults hits={sorted} entities={entities} />
              ) : (
                <ul className="space-y-2">
                  {sorted.map((hit) => (
                    <TorrentRow
                      key={`${hit.site_id}:${torrentIdOf(hit)}`}
                      hit={hit}
                      showRawTitles={view === "list"}
                    />
                  ))}
                </ul>
              )
            ) : items.length ? (
              <EmptyHint
                title="没有符合筛选条件的结果"
                hint="放宽或清除顶部的筛选条件试试。"
              />
            ) : phase === "done" ? (
              <EmptyHint
                title="没有找到匹配的资源"
                hint={
                  query.keyword
                    ? "换个关键词，或检查已配置站点是否验证通过。"
                    : "换个分类，或检查已配置站点是否验证通过。"
                }
              />
            ) : null}

            {phase === "done" && !snapshotAt && items.length > 0 && !exhausted && (
              <div className="mt-4 flex justify-center">
                <button
                  type="button"
                  disabled={loadingMore}
                  onClick={loadMore}
                  className="btn-glass px-5 py-2 text-sub font-medium disabled:opacity-50"
                >
                  {loadingMore ? "正在加载…" : `加载更多（第 ${page + 1} 页）`}
                </button>
              </div>
            )}
            {phase === "done" && exhausted && items.length > 0 && (
              <p className="mt-4 text-center text-caption text-[var(--text-faint)]">
                已加载全部 {page} 页结果
              </p>
            )}
          </div>
        </div>
      </DownloadTargetPrefContext.Provider>
    </GrabContext.Provider>
  );
}
