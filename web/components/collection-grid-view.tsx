"use client";

import {
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { PageNav } from "@/components/page-nav";
import { RefreshIcon, SearchIcon } from "@/components/icons";
import { PosterCard } from "@/components/poster-card";
import { browseDiscoveryCollection } from "@/lib/api/discover";
import type { MediaItem } from "@/lib/media-types";
import { createSessionSnapshots } from "@/lib/session-snapshot";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

const TMDB_PAGE_SIZE = 20;
const DOUBAN_FULL_LIMIT = 500;

interface CollectionGridSnapshot {
  items: MediaItem[];
  title: string;
  nextPage: number;
  totalResults: number;
  hasMore: boolean;
}

// 一个片单一条，超限时优先淘汰最久未更新的片单（见 lib/session-snapshot.ts）
const collectionGridSnapshots = createSessionSnapshots<string, CollectionGridSnapshot>(32);

function getCollectionGridSnapshot(collectionRef: string) {
  return collectionGridSnapshots.get(collectionRef);
}

function deleteCollectionGridSnapshot(collectionRef: string) {
  collectionGridSnapshots.delete(collectionRef);
}

function rememberCollectionGridSnapshot(
  collectionRef: string,
  snapshot: CollectionGridSnapshot,
) {
  collectionGridSnapshots.set(collectionRef, snapshot);
}

/**
 * 完整片单落地页（「看全部」的目的地）：用纵向网格承载大量条目。
 * collectionRef 同时携带来源、媒体类型和片单身份，页面不再为豆瓣榜单
 * 硬编码专用接口；前端只分批挂载图片节点，控制首屏开销。
 */
export function CollectionGridView({
  collectionRef,
}: {
  /** 由发现页展示清单返回的稳定片单引用。 */
  collectionRef: string;
}) {
  const initialSnapshot = getCollectionGridSnapshot(collectionRef);
  const scrollRef = useScrollRestoration(`collection:${collectionRef}`);
  const [items, setItems] = useState<MediaItem[] | null>(() => initialSnapshot?.items ?? null);
  const [title, setTitle] = useState(() => initialSnapshot?.title ?? "影视片单");
  const [query, setQuery] = useState("");
  const [selectedGenres, setSelectedGenres] = useState<string[]>([]);
  const [nextPage, setNextPage] = useState(() => initialSnapshot?.nextPage ?? 2);
  const [totalResults, setTotalResults] = useState(() => initialSnapshot?.totalResults ?? 0);
  const [hasMore, setHasMore] = useState(() => initialSnapshot?.hasMore ?? false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [errorSource, setErrorSource] = useState<"page1" | "nextPage" | null>(null);
  const loadMoreRef = useRef<HTMLDivElement>(null);
  const loadingMoreRef = useRef(false);
  const pageControllerRef = useRef<AbortController | null>(null);
  const activeCollectionRef = useRef(collectionRef);
  const generationRef = useRef(0);
  const refreshInFlightRef = useRef(false);
  const [provider, mediaType] = collectionRef.split(":");
  // 下拉刷新：+1 触发首屏 effect 重跑（重新拉第一页）。
  const [reloadKey, setReloadKey] = useState(0);
  const [pullHint, setPullHint] = useState(0);
  const pullStartRef = useRef<{ y: number; atTop: boolean } | null>(null);

  const onPullTouchStart = (event: React.TouchEvent) => {
    const root = event.currentTarget;
    pullStartRef.current = { y: event.touches[0].clientY, atTop: root.scrollTop <= 0 };
  };
  const onPullTouchMove = (event: React.TouchEvent) => {
    const start = pullStartRef.current;
    if (!start || !start.atTop) return;
    const delta = event.touches[0].clientY - start.y;
    if (delta > 0) setPullHint(Math.min(delta * 0.5, 56));
  };
  const onPullTouchEnd = () => {
    if (pullHint >= 32) {
      setPullHint(0);
      deleteCollectionGridSnapshot(collectionRef);
      setReloadKey((k) => k + 1);
    } else {
      setPullHint(0);
    }
    pullStartRef.current = null;
  };

  // 片单返回时先复用已加载窗口，避免滚动恢复目标对应的分页内容被重置掉。
  useLayoutEffect(() => {
    if (activeCollectionRef.current !== collectionRef || !items) return;
    rememberCollectionGridSnapshot(collectionRef, {
      items,
      title,
      nextPage,
      totalResults,
      hasMore,
    });
  }, [collectionRef, hasMore, items, nextPage, title, totalResults]);

  useEffect(() => {
    const controller = new AbortController();
    const currentGen = ++generationRef.current;
    refreshInFlightRef.current = true;
    const isSwitchingRef = activeCollectionRef.current !== collectionRef;
    activeCollectionRef.current = collectionRef;
    pageControllerRef.current?.abort();
    pageControllerRef.current = null;
    loadingMoreRef.current = false;
    setError(null);
    setErrorSource(null);
    setQuery("");
    setSelectedGenres([]);

    const cached = (reloadKey === 0 || isSwitchingRef) ? getCollectionGridSnapshot(collectionRef) : undefined;
    if (cached) {
      refreshInFlightRef.current = false;
      setItems(cached.items);
      setTitle(cached.title);
      setNextPage(cached.nextPage);
      setTotalResults(cached.totalResults);
      setHasMore(cached.hasMore);
      setLoadingMore(false);
      return () => {
        controller.abort();
        if (activeCollectionRef.current === collectionRef) pageControllerRef.current?.abort();
      };
    }

    if (reloadKey === 0 || isSwitchingRef) {
      setItems(null);
      setTitle("影视片单");
      setNextPage(2);
      setTotalResults(0);
      setHasMore(false);
    }
    setLoadingMore(false);
    browseDiscoveryCollection(
      collectionRef,
      TMDB_PAGE_SIZE,
      { signal: controller.signal },
      1,
      "full",
    )
      .then((collection) => {
        if (controller.signal.aborted || generationRef.current !== currentGen) return;
        setItems(collection.items);
        setTitle(collection.name);
        setTotalResults(collection.totalResults);
        setHasMore(collection.hasMore);
        setNextPage(collection.page + 1);
      })
      .catch((reason: Error) => {
        if (!controller.signal.aborted && generationRef.current === currentGen) {
          setError(reason.message || "榜单加载失败，请稍后重试");
          setErrorSource("page1");
        }
      })
      .finally(() => {
        if (generationRef.current === currentGen) {
          refreshInFlightRef.current = false;
        }
      });
    return () => {
      controller.abort();
      if (activeCollectionRef.current === collectionRef) pageControllerRef.current?.abort();
    };
  }, [collectionRef, provider, reloadKey]);

  const loadNextPage = useCallback(async () => {
    if (!hasMore || loadingMoreRef.current || refreshInFlightRef.current) return;
    const requestedCollectionRef = collectionRef;
    const requestedGen = generationRef.current;
    const controller = new AbortController();
    pageControllerRef.current = controller;
    loadingMoreRef.current = true;
    setLoadingMore(true);
    setError(null);
    try {
      const collection = await browseDiscoveryCollection(
        collectionRef,
        TMDB_PAGE_SIZE,
        { signal: controller.signal },
        nextPage,
      );
      if (
        controller.signal.aborted ||
        activeCollectionRef.current !== requestedCollectionRef ||
        generationRef.current !== requestedGen
      ) {
        return;
      }
      setItems((current) => {
        const existing = new Set((current ?? []).map((item) => item.titleRef ?? item.id));
        return [
          ...(current ?? []),
          ...collection.items.filter((item) => !existing.has(item.titleRef ?? item.id)),
        ];
      });
      setTotalResults(collection.totalResults);
      setHasMore(collection.hasMore);
      setNextPage(collection.page + 1);
    } catch (reason) {
      if (
        !controller.signal.aborted &&
        activeCollectionRef.current === requestedCollectionRef &&
        generationRef.current === requestedGen
      ) {
        setError((reason as Error).message || "下一页加载失败，请稍后重试");
        setErrorSource("nextPage");
      }
    } finally {
      if (pageControllerRef.current === controller) {
        pageControllerRef.current = null;
        loadingMoreRef.current = false;
        if (activeCollectionRef.current === requestedCollectionRef) setLoadingMore(false);
      }
    }
  }, [collectionRef, hasMore, nextPage, provider]);

  const genres = useMemo(() => {
    if (!items) return [];
    const counts = new Map<string, number>();
    for (const item of items) {
      for (const name of item.genres) counts.set(name, (counts.get(name) ?? 0) + 1);
    }
    return [...counts].sort((a, b) => b[1] - a[1]);
  }, [items]);

  // 搜索输入用延迟值参与过滤：连续敲字时先渲染输入框本身，网格的全量
  // 过滤与重渲染放到浏览器空闲时批量跟上，输入不再一字一卡
  const deferredQuery = useDeferredValue(query);
  const filtered = useMemo(() => {
    const keyword = deferredQuery.trim().toLocaleLowerCase();
    if (!items) return [];
    return items.filter((item) => {
      // 同一筛选维度采用「或」逻辑：选择科幻 + 动画即显示任一类型命中的影片。
      const matchesGenre =
        selectedGenres.length === 0 ||
        selectedGenres.some((selected) => item.genres.includes(selected));
      const matchesKeyword =
        !keyword ||
        item.title.toLocaleLowerCase().includes(keyword) ||
        item.originalTitle.toLocaleLowerCase().includes(keyword);
      return matchesGenre && matchesKeyword;
    });
  }, [items, deferredQuery, selectedGenres]);
  const filteringLoadedItems = query.trim().length > 0 || selectedGenres.length > 0;

  // TMDB 榜单按上游原生页码续载。观察根必须是应用内部的滚动容器；若使用
  // 浏览器视口，overflow 裁剪会让预取距离失效，直到哨兵真正露出才会加载。
  // 本地筛选会主动缩短网格，此时暂停续载，避免无匹配时在后台一路拉到末页。
  // 直接点「查看完整榜单」进入（SPA 导航）时数据是异步到达的：首屏期间
  // 哨兵还没渲染，若依赖里没有 items，数据到达后 observer 不会重建，
  // 上拉续载就永远不触发（刷新页面走缓存同步路径才正常）。items 进依赖，
  // 数据就绪即建观察器。
  useEffect(() => {
    const target = loadMoreRef.current;
    if (
      !target ||
      !hasMore ||
      loadingMore ||
      error ||
      filteringLoadedItems ||
      items == null ||
      refreshInFlightRef.current
    ) {
      return;
    }
    const scrollRoot = target.closest<HTMLElement>("[data-scroll-root]");
    if (!scrollRoot) return;
    const preloadDistance = Math.max(800, Math.round(scrollRoot.clientHeight * 1.25));
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) void loadNextPage();
      },
      {
        root: scrollRoot,
        rootMargin: `${preloadDistance}px 0px`,
      },
    );
    observer.observe(target);
    return () => observer.disconnect();
  }, [error, filteringLoadedItems, hasMore, items, loadNextPage, loadingMore]);

  const updateQuery = (value: string) => {
    setQuery(value);
  };

  const toggleGenre = (value: string) => {
    setSelectedGenres((current) =>
      current.includes(value)
        ? current.filter((selected) => selected !== value)
        : [...current, value],
    );
  };

  const clearGenres = () => {
    setSelectedGenres([]);
  };

  return (
    <div
      ref={scrollRef}
      data-scroll-root
      onTouchStart={onPullTouchStart}
      onTouchMove={onPullTouchMove}
      onTouchEnd={onPullTouchEnd}
      className="scroll-thin scroll-safe flex-1 overflow-y-auto px-6 pb-12 max-md:px-4"
    >
      {/* 下拉刷新提示条：触顶下拉时浮现，超过阈值松手即重新加载第一页 */}
      {pullHint > 0 && (
        <div
          className="pointer-events-none flex items-center justify-center gap-2 py-1 text-sub font-semibold text-[var(--text-muted)]"
          style={{ height: pullHint }}
          aria-hidden="true"
        >
          <RefreshIcon className="size-3.5" />
          {pullHint >= 32 ? "松开刷新" : "下拉刷新"}
        </div>
      )}
      {/* 顶栏：返回发现电影（保留豆瓣数据源视角）+ 吸顶榜单名；
          容器已有 px-6，用 -mx-6 让吸顶蒙版铺满整宽 */}
      <PageNav
        title={title}
        fallback={{
          label: mediaType === "tv" ? "发现剧集" : "发现电影",
          href: `/discover/${mediaType === "tv" ? "tv" : "movie"}?source=${provider === "douban" ? "douban" : "tmdb"}`,
        }}
        className="-mx-6 max-md:-mx-4"
      />
      <header className="mx-auto max-w-[1500px]">
        <div className="mt-1 flex flex-col justify-between gap-5 sm:flex-row sm:items-end">
          <div>
            <p className="text-sub font-semibold tracking-[0.18em] text-[var(--accent-2)]">
              {provider.toLocaleUpperCase()} COLLECTION
            </p>
            <h1 className="mt-1 text-3xl font-bold tracking-[-0.03em] text-[var(--text)]">
              {title}
            </h1>
            <p className="mt-2 text-body text-[var(--text-muted)]">
              {items
                ? provider === "tmdb"
                  ? `已加载 ${items.length} / ${totalResults.toLocaleString("zh-CN")} 部影片`
                  : hasMore
                    ? `已加载 ${items.length} 部影片`
                    : `完整收录 ${items.length} 部影片`
                : "正在读取完整榜单…"}
            </p>
          </div>
          <label className="flex h-10 w-full items-center gap-2 rounded-full border border-white/10 bg-black/25 px-4 text-[var(--text-muted)] backdrop-blur-sm sm:w-72">
            <SearchIcon className="size-4 shrink-0" />
            <input
              value={query}
              onChange={(event) => updateQuery(event.target.value)}
              placeholder={provider === "tmdb" ? "搜索已加载片名" : "搜索片名"}
              aria-label={provider === "tmdb" ? "搜索已加载的榜单片名" : "搜索榜单片名"}
              className="min-w-0 flex-1 bg-transparent text-body text-[var(--text)] outline-none placeholder:text-[var(--text-muted)]"
            />
          </label>
        </div>
        {genres.length > 0 && (
          <div className="mt-6 flex flex-wrap items-center gap-2 max-md:mt-4">
            <span className="mr-1 text-sub font-semibold text-[var(--text-muted)]">
              {provider === "tmdb" ? "已加载类型" : "类型"}
            </span>
            <button
              type="button"
              aria-pressed={selectedGenres.length === 0}
              onClick={clearGenres}
              className={`rounded-full border px-3 py-1.5 text-sub font-semibold transition ${
                selectedGenres.length === 0
                  ? "border-white/20 bg-white/15 text-white"
                  : "border-white/[0.07] bg-black/20 text-[var(--text-muted)] hover:border-white/15 hover:text-white"
              }`}
            >
              全部
            </button>
            {genres.map(([name, count]) => (
              <button
                key={name}
                type="button"
                aria-pressed={selectedGenres.includes(name)}
                onClick={() => toggleGenre(name)}
                className={`rounded-full border px-3 py-1.5 text-sub font-semibold transition ${
                  selectedGenres.includes(name)
                    ? "border-white/20 bg-white/15 text-white"
                    : "border-white/[0.07] bg-black/20 text-[var(--text-muted)] hover:border-white/15 hover:text-white"
                }`}
              >
                {name}
                <span className="tnum ml-1 text-micro opacity-55">{count}</span>
              </button>
            ))}
            {selectedGenres.length > 0 && (
              <button
                type="button"
                onClick={clearGenres}
                className="ml-1 rounded-full px-2 py-1.5 text-sub font-semibold text-[var(--accent-2)] transition hover:text-white"
              >
                清除筛选（{selectedGenres.length}）
              </button>
            )}
          </div>
        )}
      </header>

      {error && !items && (
        <div className="mx-auto mt-16 max-w-md rounded-2xl border border-white/10 bg-black/25 p-8 text-center text-body text-[var(--text-muted)]">
          <p className="mb-4">{error}</p>
          <button
            type="button"
            onClick={() => setReloadKey((k) => k + 1)}
            className="rounded-lg bg-[var(--accent-2)] px-4 py-2 font-semibold text-black transition hover:opacity-90"
          >
            重新加载
          </button>
        </div>
      )}

      {!items && !error && <CollectionSkeleton />}

      {items && (
        <main className="mx-auto mt-8 max-w-[1500px]">
          {filtered.length === 0 ? (
            <div className="py-20 text-center text-body text-[var(--text-muted)]">
              没有找到匹配的影片
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-x-4 gap-y-7 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 2xl:grid-cols-8">
              {filtered.map((item) => (
                <div key={item.id} className="min-w-0">
                  <PosterCard item={item} />
                </div>
              ))}
            </div>
          )}

          {error && (
            <div className="mt-8 flex flex-wrap items-center justify-center gap-3 rounded-xl border border-white/[0.08] bg-black/20 px-4 py-3 text-sub text-[var(--text-muted)]">
              <span>{error}</span>
              {errorSource === "page1" ? (
                <button
                  type="button"
                  onClick={() => setReloadKey((k) => k + 1)}
                  className="font-semibold text-[var(--accent-2)] transition hover:text-white"
                >
                  重试刷新
                </button>
              ) : hasMore ? (
                <button
                  type="button"
                  onClick={() => void loadNextPage()}
                  className="font-semibold text-[var(--accent-2)] transition hover:text-white"
                >
                  重试下一页
                </button>
              ) : null}
            </div>
          )}

          <div ref={loadMoreRef} className="h-px" aria-hidden="true" />
          <p className="sr-only" role="status">
            {loadingMore
              ? "正在加载下一页"
              : !hasMore && filtered.length > 0
                ? `已加载全部 ${items.length} 部影片`
                : ""}
          </p>
        </main>
      )}
    </div>
  );
}

function CollectionSkeleton() {
  return (
    <div className="mx-auto mt-8 grid max-w-[1500px] grid-cols-2 gap-x-4 gap-y-7 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 2xl:grid-cols-8">
      {Array.from({ length: 24 }, (_, index) => (
        <div
          key={index}
          className="aspect-[2/3] animate-pulse rounded-2xl bg-white/[0.05] ring-1 ring-white/10"
        />
      ))}
    </div>
  );
}
