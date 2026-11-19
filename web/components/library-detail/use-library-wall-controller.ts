"use client";

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { useToast } from "@/components/feedback";
import { usePhotoWallDensity } from "@/components/wall";
import { useVideoGalleryMode } from "@/components/video-gallery";
import {
  filterKey,
  isFilterEmpty,
  listLibraries,
  listLibraryItemIndex,
  listMissingLibraryFiles,
  listUnidentifiedLibraryFiles,
  type LibraryFilter,
  type LibraryIndexEntry,
  type LibraryItem,
  type LibraryItemOrder,
  type LibraryItemSort,
  type MediaLibrary,
  type MissingItem,
  type UnidentifiedGroup,
} from "@/lib/api/libraries";
import {
  getLibraryDetailSnapshot,
  setLibraryDetailSnapshot,
  type LibraryDetailSnapshot,
} from "@/lib/library-detail-snapshot";
import {
  hasMorePages,
  INDEXED_SORTS,
  keepOnError,
  readFilterFromUrl,
  WALL_PAGE_SIZE,
  writeFilterToUrl,
} from "@/lib/library-detail-helpers";
import {
  firstVisibleAnchorId,
  isReentryAfterAbsence,
  wallRecallScope,
} from "@/lib/library-wall-recall";
import { usePermissions } from "@/lib/permissions";
import { keepIfEqual, reconcileList } from "@/lib/poll-reconcile";
import { usePageTitle } from "@/lib/use-page-title";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";
import { useVisiblePolling } from "@/lib/use-visible-polling";
import { useWallRecall } from "@/lib/use-wall-recall";
import { PREF_TO_SORT, SORT_DIRECTIONS, useWallSortPref } from "@/lib/wall-sort";

import type { IssueTab } from "./issue-drawer";
import { useLibraryGallery } from "./use-library-gallery";
import { useWallIndexSync } from "./use-wall-index-sync";
import { useWallWindow } from "./use-wall-window";

export function useLibraryWallController(libraryId: string) {
  const snapshotKey = libraryId;
  const initialSnapshot = getLibraryDetailSnapshot(snapshotKey);
  // 重新进入时不自动回位，改由底部胶囊来问（见 library-wall-recall）
  const freshEntry = useRef(
    initialSnapshot === undefined || isReentryAfterAbsence(wallRecallScope(snapshotKey)),
  );
  const { canManageLibraries } = usePermissions();
  const [coverModalOpen, setCoverModalOpen] = useState(false);
  const [galleryPreferred, setGalleryMode] = useVideoGalleryMode();
  const galleryRef = useRef(galleryPreferred);
  galleryRef.current = galleryPreferred;
  const lastIndexGalleryRef = useRef(galleryPreferred);
  // 两种墙的锚点属性不同（海报墙条目标识 vs 图廊瓦片标识）
  const restoreScrollRef = useScrollRestoration(`library:${libraryId}`, {
    anchorAttribute: galleryPreferred ? "data-gallery-tile-id" : "data-library-item-id",
    restore: !freshEntry.current,
  });
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null);
  const scrollRef = useCallback(
    (node: HTMLDivElement | null) => {
      restoreScrollRef(node);
      setScrollElement(node);
    },
    [restoreScrollRef],
  );
  const [mediaInfoOpen, setMediaInfoOpen] = useState(false);
  const toast = useToast();
  // URL 是筛选态的唯一事实源
  const [filter, setFilter] = useState<LibraryFilter>(() => readFilterFromUrl());
  const wallFilter = useRef<LibraryFilter>(filter);
  wallFilter.current = filter;
  const filtering = !isFilterEmpty(filter);
  // 带筛选进来时不吃会话快照，避免闪现未筛选窗口
  const snapshot = filtering ? undefined : initialSnapshot;
  const [libraries, setLibraries] = useState<MediaLibrary[] | null>(snapshot?.libraries ?? null);
  const [items, setItems] = useState<LibraryItem[]>(snapshot?.items ?? []);
  const missingFileCount = items.filter((item) => item.missing).length;
  const [wallHasMore, setWallHasMore] = useState(snapshot?.wallHasMore ?? false);
  const [wallIndex, setWallIndex] = useState<LibraryIndexEntry[]>(snapshot?.wallIndex ?? []);
  const [wallOrigin, setWallOrigin] = useState(snapshot?.wallStart ?? 0);
  const [unidentified, setUnidentified] = useState<UnidentifiedGroup[]>(snapshot?.unidentified ?? []);
  const [missing, setMissing] = useState<MissingItem[]>(snapshot?.missing ?? []);
  const [failed, setFailed] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [editing, setEditing] = useState<MediaLibrary | null>(null);
  const [snapshotStale, setSnapshotStale] = useState(snapshot?.stale ?? false);
  const [issueTab, setIssueTab] = useState<IssueTab | null>(null);
  const pendingRequested = useRef(false);
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("pending") !== "1") return;
    pendingRequested.current = true;
    params.delete("pending");
    const rest = params.toString();
    window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}`);
  }, []);

  const reloadSeq = useRef(0);
  const listsSeq = useRef(0);
  const wallLoaded = useRef(snapshot?.wallLoaded ?? WALL_PAGE_SIZE);
  const wallSort = useRef<LibraryItemSort>(snapshot?.wallSort ?? "title");
  const wallOrder = useRef<LibraryItemOrder | undefined>(snapshot?.wallOrder);
  const wallCursors = useRef<(string | null)[]>(
    snapshot ? [...snapshot.wallCursors] : [null],
  );

  const wallTop = useRef<HTMLDivElement>(null);
  const filterBarTop = useRef<HTMLDivElement>(null);
  const wallGrid = useRef<HTMLDivElement>(null);

  const library = libraries?.find((l) => l.id === libraryId) ?? null;
  usePageTitle(library?.name);

  const probing = false;
  const timeline = Boolean(library && library.kind === "video");
  const [
    { pref: wallSortPref, reversed: wallSortReversed },
    setWallSortPref,
    toggleWallSortReversed,
    wallSortReady,
  ] = useWallSortPref();
  const recentFirst = wallSortPref === "added_at" && !probing;
  const effectiveSort: LibraryItemSort = probing
    ? "probing"
    : wallSortPref !== "default"
      ? PREF_TO_SORT[wallSortPref]
      : timeline
        ? "release_date"
        : "title";
  const sortAscending =
    SORT_DIRECTIONS[effectiveSort].naturalAsc !== wallSortReversed && effectiveSort !== "probing";
  const wallOrderParam: LibraryItemOrder | undefined =
    sortAscending === SORT_DIRECTIONS[effectiveSort].naturalAsc
      ? undefined
      : sortAscending
        ? "asc"
        : "desc";
  const gallerySortKey = `${effectiveSort}:${wallOrderParam ?? ""}`;
  const gallery = galleryPreferred && Boolean(library);

  const galleryState = useLibraryGallery({
    libraryId,
    gallery,
    effectiveSort,
    wallOrderParam,
    gallerySortKey,
    wallSortReady,
    wallFilter,
    wallIndex,
    wallTop,
    snapshot,
  });
  const { resetGallery } = galleryState;

  const {
    onWallGeometry,
    pendingJump,
    activeWallInitial,
    setActiveWallInitial,
  } = useWallIndexSync({
    items,
    wallIndex,
    wallOrigin,
    scrollElement,
    probing,
    wallGrid,
  });

  const snapshotRef = useRef<LibraryDetailSnapshot | null>(null);
  snapshotRef.current = libraries
    ? {
        libraries,
        items,
        wallHasMore,
        wallCursors: [...wallCursors.current],
        wallIndex,
        wallStart: wallOrigin,
        unidentified,
        missing,
        wallLoaded: wallLoaded.current,
        wallSort: wallSort.current,
        wallOrder: wallOrder.current,
        wallOffset: 0,
        galleryGroups: galleryState.galleryGroups,
        galleryHasMore: galleryState.galleryHasMore,
        galleryCursors: [...galleryState.galleryCursors.current],
        gallerySortKey: galleryState.galleryWindowSort.current,
        galleryStart: galleryState.galleryOrigin,
        galleryLoaded: galleryState.galleryLoaded.current,
        stale: snapshotStale,
      }
    : null;

  useLayoutEffect(() => {
    if (snapshotRef.current) setLibraryDetailSnapshot(snapshotKey, snapshotRef.current);
  }, [
    galleryState.galleryGroups,
    galleryState.galleryHasMore,
    galleryState.galleryOrigin,
    items,
    libraries,
    missing,
    snapshotKey,
    snapshotStale,
    unidentified,
    wallHasMore,
    wallIndex,
    wallOrigin,
  ]);

  const { loadingMore, fetchWall, reloadWall, loadMore, jumpTo } = useWallWindow({
    libraryId,
    reloadSeq,
    wallSort,
    wallOrder,
    wallFilter,
    wallCursors,
    wallLoaded,
    galleryRef,
    lastIndexGalleryRef,
    wallIndex,
    pendingJump,
    setItems,
    setWallOrigin,
    setWallHasMore,
    setWallIndex,
    setFailed,
  });

  const reload = useCallback(() => {
    const seq = ++reloadSeq.current;
    const lseq = ++listsSeq.current;
    Promise.all([
      listLibraries(undefined, canManageLibraries ? { manage: true } : undefined).then((libs) => {
        if (lseq === listsSeq.current) setLibraries((prev) => (prev ? keepIfEqual(prev, libs) : libs));
        return libs;
      }),
      fetchWall(seq),
      canManageLibraries
        ? keepOnError(listUnidentifiedLibraryFiles(libraryId))
        : Promise.resolve([]),
      canManageLibraries
        ? keepOnError(listMissingLibraryFiles(libraryId))
        : Promise.resolve([]),
    ])
      .then(([libs, [libraryItems, index], unknown, missingItems]) => {
        if (lseq !== listsSeq.current) return;
        setSnapshotStale(false);
        setFailed([unknown, missingItems].some((rows) => rows === null));
        setLibraries((prev) => (prev ? keepIfEqual(prev, libs) : libs));
        if (unknown !== null) setUnidentified((prev) => keepIfEqual(prev, unknown));
        if (missingItems !== null) setMissing((prev) => keepIfEqual(prev, missingItems));
        if (seq !== reloadSeq.current) return;
        setItems((prev) => reconcileList(prev, libraryItems, (i) => i.media_item_id));
        wallLoaded.current = Math.max(WALL_PAGE_SIZE, libraryItems.length);
        setWallHasMore(hasMorePages(wallCursors.current));
        setWallIndex((prev) => keepIfEqual(prev, index));
      })
      .catch(() => {
        if (lseq === listsSeq.current) setFailed(true);
      });
  }, [canManageLibraries, fetchWall, libraryId]);

  useEffect(() => {
    reload();
  }, [reload]);

  const applyFilter = useCallback(
    (next: LibraryFilter) => {
      wallFilter.current = next;
      setFilter(next);
      writeFilterToUrl(next);
      wallCursors.current = [null];
      wallLoaded.current = WALL_PAGE_SIZE;
      setWallOrigin(0);
      setActiveWallInitial(null);
      void reload();
      resetGallery();
      (filterBarTop.current ?? wallTop.current)?.scrollIntoView({
        block: "start",
        behavior: "instant",
      });
    },
    [reload, resetGallery, setActiveWallInitial],
  );

  const poll = useCallback(() => {
    if (!loadingMore.current) reload();
  }, [loadingMore, reload]);
  useVisiblePolling(poll, 30_000);

  const unidentifiedFiles = unidentified.reduce((n, g) => n + g.file_count, 0);
  const pendingCount = missing.length + unidentified.length;
  const pendingTab: IssueTab = missing.length > 0 ? "missing" : "unidentified";

  useEffect(() => {
    if (!pendingRequested.current || !library || !canManageLibraries) return;
    pendingRequested.current = false;
    setIssueTab(pendingTab);
  }, [library, pendingTab, canManageLibraries]);

  const defaultSort = wallSortPref === "default" || effectiveSort === "probing";
  const wallView =
    (timeline ? "wall:time" : "wall:title") +
    (recentFirst ? ":added" : defaultSort ? "" : `:${effectiveSort}`) +
    (wallOrderParam ? ":rev" : "");

  const offsetById = useMemo(
    () => new Map(items.map((item, index) => [String(item.media_item_id), wallOrigin + index])),
    [items, wallOrigin],
  );
  const galleryOffsetById = useMemo(
    () =>
      new Map(
        galleryState.galleryGroups.map((group, index) => [
          String(group.media_item_id),
          galleryState.galleryOrigin + index,
        ]),
      ),
    [galleryState.galleryGroups, galleryState.galleryOrigin],
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
  const resetWallToTop = useCallback(() => {
    scrollElement?.scrollTo({ top: 0, behavior: "instant" });
  }, [scrollElement]);
  const { recallOffset, dismissRecall } = useWallRecall({
    scope: wallRecallScope(snapshotKey, filterKey(filter)),
    view: wallView,
    scroller: scrollElement,
    enabled:
      !probing &&
      wallSortReady &&
      (gallery ? galleryState.galleryGroups.length > 0 : items.length > 0),
    offer: freshEntry.current,
    offsetAt: wallOffsetAt,
    onReenter: resetWallToTop,
  });
  const recallable =
    recallOffset !== null && recallOffset < (library?.stats.item_count ?? 0) ? recallOffset : null;
  const wideWall = library?.kind === "video";
  const workingLabelOf = useCallback((item: LibraryItem) => {
    void item;
    return undefined;
  }, []);
  const ownLibraryId = useCallback(() => libraryId, [libraryId]);

  useEffect(() => {
    if (!wallSortReady) return;
    if (wallSort.current === effectiveSort && wallOrder.current === wallOrderParam) return;
    wallSort.current = effectiveSort;
    wallOrder.current = wallOrderParam;
    wallLoaded.current = WALL_PAGE_SIZE;
    wallCursors.current = [null];
    setWallOrigin(0);
    reloadWall();
  }, [effectiveSort, wallOrderParam, wallSortReady, reloadWall]);

  useEffect(() => {
    if (lastIndexGalleryRef.current === galleryPreferred) return;
    lastIndexGalleryRef.current = galleryPreferred;
    const indexKind = INDEXED_SORTS[wallSort.current];
    if (indexKind === undefined) return;
    const seq = reloadSeq.current;
    void listLibraryItemIndex(libraryId, indexKind, wallFilter.current, wallOrder.current, {
      gallery: galleryPreferred,
    })
      .then((index) => {
        if (seq === reloadSeq.current) setWallIndex((prev) => keepIfEqual(prev, index));
      })
      .catch(() => {});
  }, [galleryPreferred, libraryId]);

  const [wallDensity, setWallDensity] = usePhotoWallDensity();
  const showRating = wallSortPref === "rating" || filter.ratingGte != null;

  return {
    canManageLibraries,
    scrollRef,
    wallTop,
    filterBarTop,
    wallGrid,
    libraries,
    library,
    items,
    wallHasMore,
    wallIndex,
    wallOrigin,
    activeWallInitial,
    unidentified,
    unidentifiedFiles,
    missing,
    missingFileCount,
    pendingCount,
    pendingTab,
    failed,
    notice,
    setNotice,
    editing,
    setEditing,
    issueTab,
    setIssueTab,
    coverModalOpen,
    setCoverModalOpen,
    mediaInfoOpen,
    setMediaInfoOpen,
    filter,
    filtering,
    applyFilter,
    reload,
    loadMore,
    jumpTo,
    probing,
    timeline,
    wallSortPref,
    setWallSortPref,
    toggleWallSortReversed,
    effectiveSort,
    sortAscending,
    gallery,
    setGalleryMode,
    wallDensity,
    setWallDensity,
    wideWall,
    showRating,
    workingLabelOf,
    ownLibraryId,
    onWallGeometry,
    recallable,
    dismissRecall,
    toast,
    ...galleryState,
  };
}
