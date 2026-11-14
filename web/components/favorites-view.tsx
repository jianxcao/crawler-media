"use client";

import { useCallback, useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState } from "react";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { useToast } from "@/components/feedback";
import { FavoriteWindow, StaleFavoriteWindow } from "@/lib/favorite-window";
import { favoriteFailures, initialFavoriteFailures } from "@/lib/favorite-failures";
import { MasonryIcon, MoreIcon, PosterGridIcon } from "@/components/icons";
import { WallLoadMore, WallLoadPrev, WallRecallPill } from "@/components/wall-chrome";
import { WallSortControl } from "@/components/library-filter-bar";
import { PosterWall } from "@/components/poster-wall";
import { PAGE_NAV_BUTTON_CLASS, PageNav } from "@/components/page-nav";
import { remeasureWalls, usePhotoWallDensity } from "@/components/wall";
import {
  GALLERY_LOAD_MARGIN,
  GALLERY_PAGE_SIZE,
  VideoGalleryLightbox,
  VideoGalleryWall,
  WallPrefItems,
  dedupeGalleryGroups,
  flattenGallery,
  useVideoGalleryGrouped,
  useVideoGalleryMode,
} from "@/components/video-gallery";
import type { LibraryGalleryGroup } from "@/lib/api/libraries";
import { galleryMarkTarget } from "@/lib/gallery-mark-target";
import {
  FAVORITE_SORTS,
  type FavoriteItem,
  type FavoriteSort,
  type FavoriteSortParams,
  listFavorites,
  listFavoritesGallery,
  setPlaybackMarks,
  seekFavoritesPosition,
} from "@/lib/api/playback";
import {
  SORT_DIRECTIONS,
  orderParam,
  useWallSortPref,
  type WallSortPref,
} from "@/lib/wall-sort";
import {
  firstVisibleAnchorId,
  isReentryAfterAbsence,
  wallRecallScope,
} from "@/lib/library-wall-recall";
import { usePageTitle } from "@/lib/use-page-title";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";
import { useWallRecall } from "@/lib/use-wall-recall";

/** 每次向服务端要的格数，与单库海报墙同一页长。 */
const PAGE_SIZE = 60;

/** 「回到上次浏览的位置」的记录键（lib/library-wall-recall.ts）。 */
const RECALL_SCOPE = wallRecallScope("favorites");

/**
 * 记录的形态口径（见 WallRecall.view）。
 *
 * 收藏页的海报墙与图廊是同一份名单、同一个排序、同一种按作品分页——「第 300 个」
 * 在两种形态里指向同一部作品，因此两种形态共用一条记录：在图廊里看到哪，切回
 * 海报墙照样跳得回去。单库页两种墙的排序不同，那边才必须分开记。
 *
 * 排序是形态的一部分：同一个 offset 在「最近收藏」与「按评分」里指向的不是同一
 * 部作品，换了排序就当作没有记录。默认档不加后缀，老记录不失效。
 */
const RECALL_VIEW = "favorites";

/** 收藏页的排序偏好键：与单库页分开记——那边的默认档是「按标题」，这边是「最近收藏」。 */
const FAVORITES_SORT_STORAGE_KEY = "crawler-media.favorites.wall-sort";

/**
 * 排序档位：默认档是这面墙独有的「最近收藏」，其余与单库海报墙同一套
 * （服务端也是同一份实现，见 items.sort_item_ids）。图廊也吃全部档位——
 * 与单库页不同，收藏图廊的接口与海报墙传同一个 sort，两种形态永远是同一份名单
 */
const SORT_OPTIONS = FAVORITE_SORTS.map((sort): readonly [WallSortPref, string] => {
  if (sort === "title") return ["title", "按标题"];
  if (sort === "year") return ["release_date", "按上映年份"];
  return ["default", "最近收藏"];
});

/** 「最近收藏」档的自然方向是新→旧，与「最近添加」同一种说法。 */
const FAVORITED_DIRECTION = { naturalAsc: false, asc: "旧→新", desc: "新→旧" } as const;

function directionOf(sort: FavoriteSort) {
  return sort === "favorited_at" ? FAVORITED_DIRECTION : SORT_DIRECTIONS[sort === "year" ? "release_date" : "title"];
}

/**
 * 「全部收藏」页离开再返回时的会话快照。
 *
 * 与单库页的 lib/library-detail-snapshot.ts 是同一件事：这面墙与作品详情页是
 * 两个路由，Next 切走时组件会被卸载。返回时若只补第一页，容器就矮到装不下
 * 离开时的滚动位置，lib/use-scroll-restoration.ts 既找不到锚点、又等不到足够
 * 的高度，超时后只能放弃——人被甩回墙首。这正是「收藏页没有滚动记忆、和别的
 * 媒体库不一样」的由来（单库页早已有快照，收藏页一直没有）。
 *
 * 一个账号只有一面收藏墙，一份模块级快照就够，不必像单库页那样按 id 存 Map。
 * 它只活在当前浏览会话里（刷新页面即失效），与滚动位置同一口径。
 */
interface FavoritesSnapshot {
  /** 海报墙已加载的整个窗口 */
  items: FavoriteItem[];
  total: number;
  /** 这份窗口在整份名单里的起点（「回到上次位置」跳过来后不为 0） */
  wallStart: number;
  wallNextCursor: string | null;
  wallLoadedEnd: number;
  /** 图床浏览模式已加载的整个窗口；空数组 = 这一次浏览没进过图廊 */
  galleryGroups: LibraryGalleryGroup[];
  galleryHasMore: boolean;
  /** 图廊窗口的起点，与 wallStart 同一个意思 */
  galleryStart: number;
  /** 已向服务端请求到第几个作品（绝对位置；没图的作品也占一组） */
  galleryLoaded: number;
  /** 两份窗口是按哪个排序取的（见 sortKey）；返回时偏好变了就不能接着用 */
  sortKey: string;
}

let snapshot: FavoritesSnapshot | null = null;

/** 本页 ⋯ 菜单的行样式（与全站各处菜单同一副长相）。 */
const ITEM_CLASS =
  "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
  "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)]";

/**
 * 整面墙钉死的框比例：2:3 竖版海报。
 *
 * 收藏是跨库的一面墙，横版封面（其他库的 16:9 抓帧）和竖版海报必然混在一起。
 * 单库页遇到这种情况会把墙切成竖横两区各自对齐，但收藏页排的是「最近收藏的
 * 在前」——按比例分区等于把收藏顺序打散，人再也找不到刚点的那部。
 *
 * 所以这里照搬最近观看那一行的做法：**框比例固定，主图按真实比例居中完整显示、
 * 同图放大模糊铺底**（PosterCardVisual 的 letterbox 分支）。每格等高、片名落在
 * 一条线上，横版封面也不会被裁掉两边。首页横滚的「我的收藏」本来就是这么排的，
 * 点「查看全部」进来的这面墙从此与它同一形态。
 */
const FAVORITES_FRAME_ASPECT = 2 / 3;

/**
 * 「全部收藏」页（/library/favorites）：当前账号收藏的全部作品。数据是
 * playback_state 里的收藏列，与 Jellyfin 客户端点的心同一份；每格的详情落点
 * 是服务端解析好的可见库。首页只横滚最近 20 部，多的到这里看。
 *
 * 与单库页一样有两种浏览形态，顶栏那颗键来回切（偏好与单库页共用一份，
 * 见 useVideoGalleryMode）：
 *   - 海报墙：同一套格子（InventoryCell）、同一种滚动加载，区别只在框比例
 *     统一钉死（见 FAVORITES_FRAME_ASPECT）；
 *   - 图床浏览：同一套瀑布流与灯箱（video-gallery），数据换成
 *     /playback/favorites/gallery——收藏作品的海报 / 剧照 / 章节场景图。
 *
 * 两种形态各自分页、互不干扰；海报墙那一份始终会拉（页头的总数与切回来时的
 * 首屏都靠它），与单库页同一处理。
 *
 * 位置记忆也与单库页同一套，两层各管一段（谁都不覆盖谁）：
 *   - 会话内：从作品详情页返回时，快照把整个已加载窗口接回来，滚动位置由
 *     lib/use-scroll-restoration.ts 自动回位，不问用户；
 *   - 跨会话：关掉页面第二天再进来，底部弹一枚胶囊问要不要回到上次的位置
 *     （lib/library-wall-recall.ts），点了就把窗口整体换到那一段。
 */
export function FavoritesView() {
  usePageTitle("我的收藏");
  const toast = useToast();
  const initialSnapshot = snapshot;
  // 本次是不是「重新进入」：首帧没有会话快照 = 冷启动 / 刷新 / 从别处进来的；
  // 或者本次页面加载期间挂过很久后台（iOS PWA 恢复应用不重新加载页面，只能
  // 靠这条认）。重新进入时不自动回位，改由底部胶囊来问——两者同时来的话，
  // 人已经在原处了，胶囊就成了指着脚下的废话
  const freshEntry = useRef(initialSnapshot === null || isReentryAfterAbsence(RECALL_SCOPE));
  // 排序偏好（含方向）：与单库页同一套钩子、同一种记法，只是各记各的键
  const [{ pref: sortPref, reversed: sortReversed }, setSortPref, toggleSortReversed, sortReady] =
    useWallSortPref(FAVORITES_SORT_STORAGE_KEY);
  // 当前真正生效的服务端排序键：默认档是「最近收藏」
  const effectiveSort: FavoriteSort =
    sortPref === "title" ? "title" : sortPref === "release_date" ? "year" : "favorited_at";
  const sortAscending = directionOf(effectiveSort).naturalAsc !== sortReversed;
  // 两种形态的请求都带同一份：不反转时不带 order，服务端按自然方向排
  const sortParams = useMemo<FavoriteSortParams>(
    () => ({
      sort: effectiveSort,
      order:
        effectiveSort === "favorited_at"
          ? sortReversed
            ? "asc"
            : undefined
          : orderParam(effectiveSort === "year" ? "release_date" : effectiveSort, sortReversed),
    }),
    [effectiveSort, sortReversed],
  );
  // ref 版：翻页 / 对账 / 补页的回调每轮读最新值，不必为换排序重建回调链
  const sortRef = useRef(sortParams);
  sortRef.current = sortParams;
  // 这份排序的指纹：窗口按它取、位置记录按它分记。默认档为空串，老记录不失效
  const sortKey = `${effectiveSort}${sortReversed ? ":rev" : ""}`;
  const [galleryPreferred, setGalleryMode] = useVideoGalleryMode();
  const [galleryGrouped, setGalleryGrouped] = useVideoGalleryGrouped();
  const [density, setDensity] = usePhotoWallDensity();
  // 两种墙的滚动锚点不同：海报墙一格一个条目，图廊一部作品十几张图按瓦片认
  const restoreScrollRef = useScrollRestoration("library:favorites", {
    anchorAttribute: galleryPreferred ? "data-gallery-tile-id" : "data-library-item-id",
    restore: !freshEntry.current,
  });
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null);
  // 滚动恢复、位置记录与前置加载的滚动补偿共用同一个真实滚动容器，合并
  // callback ref，免得几套监听器各自去猜 window/document
  const scrollRef = useCallback(
    (node: HTMLDivElement | null) => {
      restoreScrollRef(node);
      setScrollElement(node);
    },
    [restoreScrollRef],
  );
  // 首帧就把上次的窗口接回来，滚动恢复才有落脚的高度
  const [items, setItems] = useState<FavoriteItem[] | null>(initialSnapshot?.items ?? null);
  const [total, setTotal] = useState(initialSnapshot?.total ?? 0);
  // 当前窗口在整份名单里的起点：0 = 墙首；「回到上次位置」跳过来后不为 0
  const [wallStart, setWallStart] = useState(initialSnapshot?.wallStart ?? 0);
  const [failures, reportRead] = useReducer(favoriteFailures, initialFavoriteFailures);
  const wallPages = useMemo(() => new FavoriteWindow(listFavorites, seekFavoritesPosition), []);
  const galleryPages = useMemo(() => new FavoriteWindow(listFavoritesGallery, seekFavoritesPosition), []);
  const [wallNextCursor, setWallNextCursor] = useState(initialSnapshot?.wallNextCursor ?? null);
  const wallLoadedEnd = useRef(initialSnapshot?.wallLoadedEnd ?? 0);
  const wallRequest = useRef(0);
  const galleryRequest = useRef(0);
  useEffect(() => () => {
    wallPages.invalidate();
    galleryPages.invalidate();
  }, [wallPages, galleryPages]);
  // 起点的 ref 版：几个回调每次都读最新值，不必为它重建回调链
  const wallOffset = useRef(initialSnapshot?.wallStart ?? 0);
  // 翻页请求进行中：哨兵重新观察时不重复发同一页
  const loading = useRef(false);
  // 进这一屏时快照里有多少格（挂载后本组件自己就会改写 snapshot，得先定格）
  const restoredCount = useRef(initialSnapshot?.items.length ?? 0);
  // 海报墙顶部的锚：跳转后滚回墙首，位置记录也按它量首个可见格
  const wallTop = useRef<HTMLDivElement>(null);

  /** Cursor append keeps the raw selected position, independently of UI dedup. */
  const load = useCallback(async (start: number) => {
    if (loading.current || loadingPrev.current) return;
    loading.current = true;
    const request = ++wallRequest.current;
    try {
      const page = await wallPages.page(start, PAGE_SIZE, sortRef.current);
      if (request !== wallRequest.current) return;
      if (page.total !== null) setTotal(page.total);
      wallLoadedEnd.current = start + page.items.length;
      setWallNextCursor(page.next_cursor);
      setItems((previous) => {
        const seen = new Set((previous ?? []).map((item) => item.media_item_id));
        return [...(previous ?? []), ...page.items.filter((item) => !seen.has(item.media_item_id))];
      });
      reportRead({ channel: "wall", failed: false });
    } catch (error) {
      if (!(error instanceof StaleFavoriteWindow) && request === wallRequest.current) reportRead({ channel: "wall", failed: true });
    } finally {
      if (request === wallRequest.current) loading.current = false;
    }
  }, [wallPages]);

  const reload = useCallback(async (loadedCount: number) => {
    if (loading.current) return;
    loading.current = true;
    const request = ++wallRequest.current;
    try {
      const page = await wallPages.refresh(wallOffset.current, loadedCount, PAGE_SIZE, sortRef.current);
      if (request !== wallRequest.current) return;
      wallOffset.current = page.start;
      wallLoadedEnd.current = page.start + page.items.length;
      setWallStart(page.start);
      setWallNextCursor(page.next_cursor);
      if (page.total !== null) setTotal(page.total);
      setItems(page.items);
      reportRead({ channel: "wall", failed: false });
    } catch (error) {
      if (!(error instanceof StaleFavoriteWindow) && request === wallRequest.current) reportRead({ channel: "wall", failed: true });
    } finally {
      if (request === wallRequest.current) loading.current = false;
    }
  }, [wallPages]);

  const loaded = items?.length ?? 0;
  const hasMore = items !== null && wallNextCursor !== null;
  const ownerLibraryOf = useCallback((item: FavoriteItem) => item.library_id, []);
  const loadMore = useCallback(() => { void load(wallLoadedEnd.current); }, [load]);

  /* —— 向上补页 ——
     「回到上次位置」会把整个窗口换成从那一段开始的一页，窗口起点因此不为 0：
     上方明明还有作品，往上滑却是一堵墙。这里补上反方向的分页——墙顶的哨兵
     提前 600px 触发，把上一页接到已加载内容前面（与单库页同一条路）。 */
  const loadingPrev = useRef(false);
  // 前置加载前滚动容器的高度；null = 本次 items 变化不是前置加载，不必补偿
  const prependFrom = useRef<number | null>(null);
  const loadPrev = useCallback(() => {
    if (loadingPrev.current || loading.current) return;
    const until = wallOffset.current;
    if (until <= 0) return;
    loadingPrev.current = true;
    const request = ++wallRequest.current;
    const from = Math.max(0, until - PAGE_SIZE);
    wallPages.page(from, until - from, sortRef.current)
      .then((page) => {
        if (request !== wallRequest.current) return;
        reportRead({ channel: "wall", failed: false });
        if (page.items.length === 0) return;
        wallOffset.current = from;
        prependFrom.current = scrollElement?.scrollHeight ?? null;
        if (page.total !== null) setTotal(page.total);
        setWallStart(from);
        setItems((current) => {
          const seen = new Set((current ?? []).map((item) => item.media_item_id));
          return [...page.items.filter((item) => !seen.has(item.media_item_id)), ...(current ?? [])];
        });
      })
      .catch((error) => {
        if (!(error instanceof StaleFavoriteWindow) && request === wallRequest.current) reportRead({ channel: "wall", failed: true });
      })
      .finally(() => { if (request === wallRequest.current) loadingPrev.current = false; });
  }, [scrollElement, wallPages]);

  // 前置加载的滚动补偿：墙长高了多少就把 scrollTop 加回多少。放 layout effect
  // 里在绘制前完成；补完立刻让虚拟化窗口重量一次——滚动事件要到下一帧才来，
  // 不补这一下会闪一帧空墙
  useLayoutEffect(() => {
    const before = prependFrom.current;
    prependFrom.current = null;
    if (before === null || !scrollElement) return;
    const grown = scrollElement.scrollHeight - before;
    if (grown <= 0) return;
    scrollElement.scrollTop += grown;
    remeasureWalls();
  }, [items, scrollElement]);

  /**
   * 跳到整份名单里的某个位置：换掉整个窗口（而不是从头一页页追加过去），
   * 此后向下照常滚动加载，向上由墙顶哨兵把上文补回来（loadPrev）。
   */
  const jumpTo = useCallback((start: number) => {
    wallPages.invalidate();
    const request = ++wallRequest.current;
    loading.current = true;
    loadingPrev.current = true;
    wallPages.page(start, PAGE_SIZE, sortRef.current)
      .then((page) => {
        if (request !== wallRequest.current) return;
        wallOffset.current = start;
        wallLoadedEnd.current = start + page.items.length;
        setWallNextCursor(page.next_cursor);
        if (page.total !== null) setTotal(page.total);
        setWallStart(start);
        setItems(page.items);
        reportRead({ channel: "wall", failed: false });
        wallTop.current?.scrollIntoView({ block: "start", behavior: "instant" });
      })
      .catch((error) => {
        if (!(error instanceof StaleFavoriteWindow) && request === wallRequest.current) reportRead({ channel: "wall", failed: true });
      })
      .finally(() => {
        if (request === wallRequest.current) { loading.current = false; loadingPrev.current = false; }
      });
  }, [wallPages]);

  /**
   * 海报墙的窗口该按哪个排序取：
   *   - 偏好还没从 storage 读出来（sortReady 为假）先按兵不动——这一帧的排序是
   *     默认值，照它拉一遍再按真正的偏好重拉，人会被甩回墙首；
   *   - 首次进来：从墙首拉第一页；从详情页返回（快照带回窗口且排序没变）：
   *     按已加载的页数整窗对账；
   *   - 换了排序：整个窗口换掉、回到墙首（jumpTo(0)），与单库页同一处理。
   */
  const wallWindowSort = useRef<string | null>(initialSnapshot?.sortKey ?? null);
  useEffect(() => {
    if (!sortReady) return;
    const previous = wallWindowSort.current;
    wallWindowSort.current = sortKey;
    if (previous === null) void load(0);
    else if (previous !== sortKey) jumpTo(0);
    else if (restoredCount.current > 0) void reload(restoredCount.current);
    else void load(0);
  }, [sortReady, sortKey, load, reload, jumpTo]);

  // —— 图床浏览模式 —— //
  const [galleryGroups, setGalleryGroups] = useState<LibraryGalleryGroup[]>(
    initialSnapshot?.galleryGroups ?? [],
  );
  const [galleryHasMore, setGalleryHasMore] = useState(initialSnapshot?.galleryHasMore ?? false);
  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null);
  // 这份图廊窗口的起点（与海报墙的 wallStart 同一个意思）
  const galleryStart = useRef(initialSnapshot?.galleryStart ?? 0);
  // 已请求到的作品数（绝对位置，按页长推进，不按拿到的组数——没图的作品也占一组）
  const galleryLoaded = useRef(initialSnapshot?.galleryLoaded ?? 0);
  const galleryLoading = useRef(false);
  const galleryEntries = useMemo(() => flattenGallery(galleryGroups), [galleryGroups]);

  const loadMoreGallery = useCallback(() => {
    if (galleryLoading.current) return;
    galleryLoading.current = true;
    const request = ++galleryRequest.current;
    const start = galleryLoaded.current;
    galleryPages.page(start, GALLERY_PAGE_SIZE, sortRef.current)
      .then((page) => {
        if (request !== galleryRequest.current) return;
        galleryLoaded.current = start + page.items.length;
        setGalleryGroups((current) => dedupeGalleryGroups([...current, ...page.items]));
        setGalleryHasMore(page.next_cursor !== null);
        if (page.total !== null) setTotal(page.total);
        reportRead({ channel: "gallery", failed: false });
      })
      .catch((error) => {
        if (!(error instanceof StaleFavoriteWindow) && request === galleryRequest.current) reportRead({ channel: "gallery", failed: true });
      })
      .finally(() => { if (request === galleryRequest.current) galleryLoading.current = false; });
  }, [galleryPages]);

  const refreshGallery = useCallback(() => {
    const start = galleryStart.current;
    const count = galleryLoaded.current - start;
    if (count <= 0 || galleryLoading.current) return;
    galleryLoading.current = true;
    const request = ++galleryRequest.current;
    galleryPages.refresh(start, count, GALLERY_PAGE_SIZE, sortRef.current)
      .then((page) => {
        if (request !== galleryRequest.current) return;
        galleryStart.current = page.start;
        galleryLoaded.current = page.start + page.items.length;
        setGalleryGroups(dedupeGalleryGroups(page.items));
        setGalleryHasMore(page.next_cursor !== null);
        if (page.total !== null) setTotal(page.total);
        reportRead({ channel: "gallery", failed: false });
      })
      .catch((error) => {
        if (!(error instanceof StaleFavoriteWindow) && request === galleryRequest.current) reportRead({ channel: "gallery", failed: true });
      })
      .finally(() => { if (request === galleryRequest.current) galleryLoading.current = false; });
  }, [galleryPages]);

  const jumpGalleryTo = useCallback((start: number) => {
    galleryPages.invalidate();
    const request = ++galleryRequest.current;
    galleryLoading.current = true;
    galleryPages.page(start, GALLERY_PAGE_SIZE, sortRef.current)
      .then((page) => {
        if (request !== galleryRequest.current) return;
        galleryStart.current = start;
        galleryLoaded.current = start + page.items.length;
        setGalleryGroups(dedupeGalleryGroups(page.items));
        setGalleryHasMore(page.next_cursor !== null);
        if (page.total !== null) setTotal(page.total);
        reportRead({ channel: "gallery", failed: false });
        wallTop.current?.scrollIntoView({ block: "start", behavior: "smooth" });
      })
      .catch((error) => {
        if (!(error instanceof StaleFavoriteWindow) && request === galleryRequest.current) reportRead({ channel: "gallery", failed: true });
      })
      .finally(() => { if (request === galleryRequest.current) galleryLoading.current = false; });
  }, [galleryPages]);

  // 这份图廊窗口是按哪个排序取的；null = 还没取过（快照没带图廊窗口回来）
  const galleryWindowSort = useRef<string | null>(
    initialSnapshot?.galleryGroups.length ? initialSnapshot.sortKey : null,
  );
  // 切进图廊（或进页面时偏好就在图廊）：第一次、或换了排序，从头拉一页；快照
  // 带回窗口且排序没变时**不能**清空重拉——只补第一页的话容器矮到装不下离开
  // 时的滚动位置，人被甩回墙首（与单库页同一处理），改成按已加载的页数整窗对账。
  // 海报墙那一份照常自己拉，切回去时首屏已经在手上。偏好没读出来前同样按兵不动
  useEffect(() => {
    if (!galleryPreferred || !sortReady) return;
    if (galleryWindowSort.current === sortKey && galleryLoaded.current > 0) {
      refreshGallery();
      return;
    }
    galleryWindowSort.current = sortKey;
    galleryPages.invalidate();
    galleryRequest.current += 1;
    galleryLoading.current = false;
    galleryStart.current = 0;
    galleryLoaded.current = 0;
    setGalleryGroups([]);
    setGalleryHasMore(false);
    loadMoreGallery();
  }, [galleryPreferred, sortReady, sortKey, loadMoreGallery, refreshGallery, galleryPages]);

  /**
   * 灯箱里点心：取消收藏后**不把瓦片抽走**——正在看的这张图连同它所在的那一段
   * 会当场消失，翻页下标全乱。墙上的心跟着灭掉即可，下次进这一页时它自然不在了。
   */
  const toggleGalleryFavorite = useCallback(
    async (mediaItemId: string, next: boolean) => {
      const patch = (value: boolean) =>
        setGalleryGroups((current) =>
          current.map((g) => (g.media_item_id === mediaItemId ? { ...g, is_favorite: value } : g)),
        );
      patch(next);
      try {
        const marks = await setPlaybackMarks(
          galleryMarkTarget(mediaItemId),
          { favorite: next },
        );
        patch(marks.is_favorite);
      } catch (e) {
        patch(!next);
        toast.error(e instanceof Error ? e.message : "收藏失败，请稍后重试");
      }
    },
    [toast],
  );

  // 布局提交后就更新快照，路由切换的下一棵树可以在首帧直接读到它；只在卸载
  // 时写是来不及的——新路由的首次 render 可能早于被动 effect 的 cleanup
  useLayoutEffect(() => {
    if (items === null) return;
    snapshot = {
      items,
      total,
      wallStart,
      wallNextCursor,
      wallLoadedEnd: wallLoadedEnd.current,
      galleryGroups,
      galleryHasMore,
      galleryStart: galleryStart.current,
      galleryLoaded: galleryLoaded.current,
      // 窗口按哪个排序取的就记哪个：偏好读出来之前窗口还是快照那份，记它原来的键
      sortKey: wallWindowSort.current ?? sortKey,
    };
  }, [galleryGroups, galleryHasMore, items, total, wallStart, sortKey, wallNextCursor]);

  // 收藏一部都没有时不给切换键：两种形态都是空页，多一颗键只会让人以为点了没反应
  const empty = items !== null && items.length === 0;
  const gallery = galleryPreferred && !empty;

  /* —— 「回到上次浏览的位置」（lib/library-wall-recall.ts）——
     收藏攒到几百部之后，滑到第几十屏是常态，关掉页面第二天再进来又从墙首
     开始。底部弹一枚胶囊问一句：要跳回去点它，不要就继续滑——滑够一屏胶囊
     自己让位，从那一刻起记的是新位置。会话内从详情页返回不弹（滚动恢复已经
     自动回位）。 */
  // 作品 id → 它在整份名单里的绝对位置。滚动时按首个可见格反查（图廊按瓦片
  // 所属的作品），DOM 上只挂 id，不必给每一格再算一遍下标
  const offsetById = useMemo(
    () =>
      new Map((items ?? []).map((item, index) => [String(item.media_item_id), wallStart + index])),
    [items, wallStart],
  );
  // galleryStart 只在窗口被整体换掉时变，与 galleryGroups 同一时刻，不必进依赖
  const galleryOffsetById = useMemo(
    () =>
      new Map(
        galleryGroups.map((group, index) => [
          String(group.media_item_id),
          galleryStart.current + index,
        ]),
      ),
    [galleryGroups],
  );
  const wallOffsetAt = useCallback(() => {
    const wall = wallTop.current;
    if (!wall || !scrollElement) return null;
    const id = firstVisibleAnchorId(
      wall,
      gallery ? "data-gallery-item-id" : "data-library-item-id",
      scrollElement.getBoundingClientRect().top,
    );
    return id === null ? null : ((gallery ? galleryOffsetById : offsetById).get(id) ?? null);
  }, [gallery, galleryOffsetById, offsetById, scrollElement]);
  // 久别回归时把这一屏复位：页面没重新加载，人还停在离开时的位置上，不回到
  // 顶部的话胶囊指着的就是脚下这一格。瞬时而不是平滑——这是「重新进入」，
  // 不是一次导航，几万像素的平滑动画只会让人以为页面失控了
  const resetWallToTop = useCallback(() => {
    scrollElement?.scrollTo({ top: 0, behavior: "instant" });
  }, [scrollElement]);
  const { recallOffset, dismissRecall } = useWallRecall({
    scope: RECALL_SCOPE,
    view: sortKey ? `${RECALL_VIEW}:${sortKey}` : RECALL_VIEW,
    scroller: scrollElement,
    enabled: gallery ? galleryGroups.length > 0 : loaded > 0,
    offer: freshEntry.current,
    offsetAt: wallOffsetAt,
    onReenter: resetWallToTop,
  });
  // 记录可能指向已经不存在的位置（收藏被大批取消），跳过去只会是一面空墙
  const recallable = recallOffset !== null && recallOffset < total ? recallOffset : null;

  return (
    <div ref={scrollRef} className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
      <PageNav
        title="我的收藏"
        fallback={{ label: "媒体库", href: "/library" }}
        actions={
          empty ? undefined : (
            <>
              {/* 与单库页顶栏同一颗键、同一副图标：画的是**点过去会变成的那面墙** */}
              <button
                type="button"
                title={gallery ? "回到海报墙" : "图床浏览"}
                aria-label={gallery ? "回到海报墙" : "图床浏览"}
                aria-pressed={gallery}
                onClick={() => {
                  // 两种模式的灯箱翻的不是同一份列表，下标不能沿用
                  setLightboxIndex(null);
                  setGalleryMode(!gallery);
                }}
                className={`${PAGE_NAV_BUTTON_CLASS} ${gallery ? "bg-black/55 text-white" : ""}`}
              >
                {gallery ? (
                  <PosterGridIcon className="size-[18px] max-md:size-[22px]" />
                ) : (
                  <MasonryIcon className="size-[18px] max-md:size-[22px]" />
                )}
              </button>
              {/* 看图的两个偏好；海报墙上没有可调的，整个菜单也就不出现 */}
              {gallery && (
                <DropdownMenu.Root>
                  <DropdownMenu.Trigger asChild>
                    <button
                      type="button"
                      aria-label="浏览设置"
                      className={`${PAGE_NAV_BUTTON_CLASS} data-[state=open]:bg-black/55 data-[state=open]:text-white`}
                    >
                      <MoreIcon className="size-[18px] max-md:size-[22px]" />
                    </button>
                  </DropdownMenu.Trigger>
                  <DropdownMenu.Portal>
                    <DropdownMenu.Content
                      align="end"
                      sideOffset={6}
                      collisionPadding={12}
                      className="menu-surface z-50 min-w-[11rem] p-1"
                    >
                      <WallPrefItems
                        grouped={galleryGrouped}
                        onGroupedChange={setGalleryGrouped}
                        density={density}
                        onDensityChange={setDensity}
                        itemClass={ITEM_CLASS}
                      />
                    </DropdownMenu.Content>
                  </DropdownMenu.Portal>
                </DropdownMenu.Root>
              )}
            </>
          )
        }
      />
      <div className="px-6 max-md:px-4">
        <h2 className="text-on-image truncate text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[20px]">
          我的收藏
        </h2>
        <p className="text-on-image mt-1.5 truncate text-ui text-[var(--text-muted)] max-md:text-sub">
          {items === null
            ? "正在读取收藏…"
            : total > 0
              ? `${total} 部作品 · 与 Jellyfin 客户端里点的心同一份`
              : "还没有收藏。在影片页点心，或在 Jellyfin 客户端里收藏，都会出现在这里。"}
        </p>
        {/* 排序：与单库页同一颗控件、同一套档位（默认档叫「最近收藏」）。「按什么排」
            看墙是看不出来的，所以当前值必须挂在外面，不能收进 ⋯ 菜单。两种形态
            共用同一份排序——图廊的接口与海报墙传同一个 sort，切来切去是同一份名单 */}
        {!empty && items !== null && (
          <div className="mt-3 flex items-center">
            <WallSortControl
              value={effectiveSort === "title" ? "title" : effectiveSort === "year" ? "release_date" : "default"}
              options={SORT_OPTIONS}
              onChange={setSortPref}
              disabled={!sortReady}
              direction={{
                ascending: sortAscending,
                label: directionOf(effectiveSort)[sortAscending ? "asc" : "desc"],
                onToggle: toggleSortReversed,
              }}
            />
          </div>
        )}

        {failures.wall && items === null && (
          <div className="mt-16 flex flex-col items-center gap-3 text-center">
            <p className="text-ui text-[var(--text-muted)]">收藏加载失败</p>
            <button
              type="button"
              onClick={() => void load(0)}
              className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
            >
              重试
            </button>
          </div>
        )}

        {(gallery ? failures.gallery : failures.wall) && items !== null && (
          <div role="alert" className="mt-4 text-ui text-[var(--text-muted)]">
            收藏加载失败，已保留当前内容。
            <button type="button" className="ml-3 text-white" onClick={() => {
              if (gallery) {
                if (galleryLoaded.current > galleryStart.current) refreshGallery();
                else loadMoreGallery();
              } else void reload(Math.max(loaded, PAGE_SIZE));
            }}>重试</button>
          </div>
        )}

        {items !== null && items.length > 0 && (
          <>
            {/* overflow-anchor:none：向上补页后墙会长高，浏览器自带的滚动锚定
                会跟着自己补一次 scrollTop，与我们按长高量做的补偿叠加就是跳两下
                （何况 Safari 根本没有滚动锚定）。这一段的位置全部自己算 */}
            <div ref={wallTop} className="mt-6 [overflow-anchor:none]">
              {/* 墙顶还有上文时向上补页：跳「回到上次位置」之后仍然能往上滑 */}
              {!gallery && <WallLoadPrev start={wallStart} onReach={loadPrev} />}
              {gallery ? (
                <VideoGalleryWall
                  groups={galleryGroups}
                  density={density}
                  grouped={galleryGrouped}
                  onOpen={setLightboxIndex}
                />
              ) : (
                <PosterWall
                  items={items}
                  libraryIdOf={ownerLibraryOf}
                  wide={false}
                  frameAspect={FAVORITES_FRAME_ASPECT}
                />
              )}
            </div>
            {/* 两种形态各自分页，哨兵按当前模式接线（图廊按作品数计） */}
            <WallLoadMore
              hasMore={gallery ? galleryHasMore : hasMore}
              loaded={gallery ? galleryLoaded.current - galleryStart.current : loaded}
              start={gallery ? galleryStart.current : wallStart}
              total={total}
              onReach={gallery ? loadMoreGallery : loadMore}
              rootMargin={gallery ? GALLERY_LOAD_MARGIN : undefined}
            />
          </>
        )}
      </div>
      {/* 上次滑到哪：进来时问一句要不要跳回去，不理它、往下滑一屏就自己消失 */}
      {recallable !== null && (
        <WallRecallPill
          onJump={() => {
            dismissRecall();
            if (gallery) jumpGalleryTo(recallable);
            else jumpTo(recallable);
          }}
          onDismiss={dismissRecall}
        />
      )}
      {gallery && lightboxIndex !== null && galleryEntries[lightboxIndex] && (
        <VideoGalleryLightbox
          entries={galleryEntries}
          index={lightboxIndex}
          hasMore={galleryHasMore}
          onIndexChange={setLightboxIndex}
          onReachEnd={loadMoreGallery}
          onToggleFavorite={toggleGalleryFavorite}
          onClose={() => setLightboxIndex(null)}
        />
      )}
    </div>
  );
}
