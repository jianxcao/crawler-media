"use client";

import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";

import { useToast } from "@/components/feedback";
import {
  dedupeGalleryGroups,
  flattenGallery,
  GALLERY_PAGE_SIZE,
  useVideoGalleryGrouped,
} from "@/components/video-gallery";
import {
  type LibraryGalleryGroup,
  type LibraryIndexEntry,
  type LibraryItemOrder,
  type LibraryItemSort,
  listLibraryGallery,
} from "@/lib/api/libraries";
import { setPlaybackMarks } from "@/lib/api/playback";
import { galleryMarkTarget } from "@/lib/gallery-mark-target";
import type { LibraryDetailSnapshot } from "@/lib/library-detail-snapshot";
import type { LibraryFilter } from "@/lib/library-filter";
import { wallBucketAtOffset, wallBucketOffsets } from "@/lib/library-wall-index";

export function useLibraryGallery({
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
}: {
  libraryId: string;
  gallery: boolean;
  effectiveSort: LibraryItemSort;
  wallOrderParam: LibraryItemOrder | undefined;
  gallerySortKey: string;
  wallSortReady: boolean;
  wallFilter: RefObject<LibraryFilter>;
  wallIndex: LibraryIndexEntry[];
  wallTop: RefObject<HTMLDivElement | null>;
  snapshot: LibraryDetailSnapshot | undefined;
}) {
  const toast = useToast();
  const [galleryGroups, setGalleryGroups] = useState<LibraryGalleryGroup[]>(
    snapshot?.galleryGroups ?? [],
  );
  const [galleryHasMore, setGalleryHasMore] = useState(snapshot?.galleryHasMore ?? false);
  // 当前图廊窗口在整份排序里的起点（0 = 墙首；「回到上次位置」跳过来后不为 0）。
  // 图廊也走**游标链**：galleryCursors[i] 是第 i 页的起始 cursor（第 0 项 = 窗口起点）
  const galleryCursors = useRef<(string | null)[]>(
    snapshot ? [...snapshot.galleryCursors] : [null],
  );
  const galleryWindowSort = useRef<string | null>(snapshot?.gallerySortKey ?? null);
  // 图廊窗口第一组在整份排序里的绝对位置（前缀模型下 0，跳档后是该档起点）
  const [galleryOrigin, setGalleryOrigin] = useState(snapshot?.galleryStart ?? 0);
  // 已请求到第几个条目（绝对位置，按页长推进，不按拿到的组数——没图的条目也占一组）
  const galleryLoaded = useRef(snapshot?.galleryLoaded ?? 0);

  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null);
  const closeLightbox = useCallback(() => setLightboxIndex(null), []);
  // 按作品分段，还是整库的图混成一条瀑布流（⋯ 菜单里切换）
  const [galleryGrouped, setGalleryGrouped] = useVideoGalleryGrouped();
  const galleryLoading = useRef(false);
  const galleryGeneration = useRef(0);
  const galleryEntries = useMemo(() => flattenGallery(galleryGroups), [galleryGroups]);

  const loadMoreGallery = useCallback(() => {
    if (galleryLoading.current) return;
    const next = galleryCursors.current.length - 1;
    const cursor = galleryCursors.current[next] ?? null;
    if (next > 0 && cursor === null) {
      setGalleryHasMore(false);
      return;
    }
    galleryLoading.current = true;
    const gen = galleryGeneration.current;
    listLibraryGallery(libraryId, {
      limit: GALLERY_PAGE_SIZE,
      cursor,
      sort: effectiveSort,
      order: wallOrderParam,
      filter: wallFilter.current,
    })
      .then((page) => {
        if (gen !== galleryGeneration.current) return;
        galleryCursors.current[next] = cursor;
        galleryCursors.current[next + 1] = page.next_cursor;
        galleryLoaded.current += page.groups.length;
        setGalleryGroups((current) => dedupeGalleryGroups([...current, ...page.groups]));
        setGalleryHasMore(page.next_cursor !== null);
      })
      .catch(() => {
        if (gen === galleryGeneration.current) setGalleryHasMore(false);
      })
      .finally(() => {
        if (gen === galleryGeneration.current) {
          galleryLoading.current = false;
        }
      });
  }, [libraryId, effectiveSort, wallOrderParam, wallFilter]);

  /**
   * 按已加载的页数重拉整个图廊窗口：从快照恢复出来的窗口是离开这一屏时的旧
   * 数据（比如在详情页点了心，返回时角标要跟着变），拿到结果整体替换。
   * 不缩窗口、不动滚动位置；失败就留着旧窗口，下次返回再对账。
   */
  const refreshGallery = useCallback(() => {
    const wanted = galleryLoaded.current;
    if (wanted <= 0 || galleryLoading.current) return;
    galleryLoading.current = true; // 对账期间别让滚动哨兵同时追加下一页
    const gen = galleryGeneration.current;
    const cursors: (string | null)[] = [galleryCursors.current[0] ?? null];
    const filter = wallFilter.current;
    const fetchAt = (page: number, cursor: string | null) =>
      listLibraryGallery(libraryId, {
        filter,
        limit: GALLERY_PAGE_SIZE,
        cursor,
        sort: effectiveSort,
        order: wallOrderParam,
      }).then((result) => {
        cursors[page] = cursor;
        cursors[page + 1] = result.next_cursor;
        return result;
      });
    const pages = Math.max(1, Math.ceil(wanted / GALLERY_PAGE_SIZE));
    void (async () => {
      const all: { groups: LibraryGalleryGroup[]; next_cursor: string | null }[] = [];
      for (let page = 0; page < pages; page += 1) {
        if (gen !== galleryGeneration.current) return;
        if (page > 0 && cursors[page] === null) break;
        all.push(await fetchAt(page, cursors[page] ?? null));
      }
      if (gen !== galleryGeneration.current) return;
      const end = cursors.indexOf(null, 1);
      cursors.length = end < 0 ? Math.min(cursors.length, pages + 1) : end + 1;
      galleryCursors.current = cursors;
      galleryLoaded.current = all.reduce((sum, page) => sum + page.groups.length, 0);
      setGalleryGroups(dedupeGalleryGroups(all.flatMap((page) => page.groups)));
      setGalleryHasMore((all.at(-1)?.next_cursor ?? null) !== null);
    })()
      .catch(() => undefined)
      .finally(() => {
        if (gen === galleryGeneration.current) galleryLoading.current = false;
      });
  }, [libraryId, effectiveSort, wallOrderParam, wallFilter]);

  /**
   * 图廊跳到整份排序里的某个位置（「回到上次位置」在图廊形态下走这里）。
   *
   * 与海报墙的 `jumpTo` 同一套：取目标档的不透明游标，**一次**请求就落在该档第一组；
   * 只有目标比一页更深时才顺着链再走。窗口起点（`galleryOrigin`）随之改变，向上滚动
   * 同样没有 prepend。
   */
  const jumpGalleryTo = useCallback(
    (offset: number) => {
      const target = Math.max(0, offset);
      if (galleryLoading.current) return;
      galleryLoading.current = true; // 跳转期间挡住滚动哨兵与对账
      const gen = galleryGeneration.current;
      const bucket = wallBucketAtOffset(wallIndex, target);
      const bucketStart = bucket < 0 ? 0 : wallBucketOffsets(wallIndex)[bucket];
      const cursor = bucket < 0 ? null : wallIndex[bucket].cursor;
      const fetchAt = (from: string | null) =>
        listLibraryGallery(libraryId, {
          limit: GALLERY_PAGE_SIZE,
          cursor: from,
          sort: effectiveSort,
          order: wallOrderParam,
          filter: wallFilter.current,
        });
      void (async () => {
        const first = await fetchAt(cursor);
        let groups = first.groups;
        let next = first.next_cursor;
        const chain: (string | null)[] = [cursor];
        const seenCursors = new Set<string>();
        if (cursor !== null) seenCursors.add(cursor);
        if (first.groups.length === 0 || (next !== null && seenCursors.has(next))) {
          next = null;
        }
        while (next !== null && target - bucketStart >= groups.length) {
          if (gen !== galleryGeneration.current) return;
          seenCursors.add(next);
          chain.push(next);
          const page = await fetchAt(next);
          groups = [...groups, ...page.groups];
          const nextCursor = page.next_cursor;
          if (page.groups.length === 0 || (nextCursor !== null && seenCursors.has(nextCursor))) {
            next = null;
            break;
          }
          next = nextCursor;
        }
        chain.push(next);
        if (gen !== galleryGeneration.current) return;
        galleryCursors.current = chain;
        galleryLoaded.current = groups.length;
        setGalleryOrigin(bucketStart);
        setGalleryGroups(dedupeGalleryGroups(groups));
        setGalleryHasMore(next !== null);
        // 图廊没有虚拟化坐标：滚到墙首，目标那组就在视口下方不远处
        wallTop.current?.scrollIntoView({ block: "start", behavior: "smooth" });
      })()
        .catch(() => {})
        .finally(() => {
          if (gen === galleryGeneration.current) galleryLoading.current = false;
        });
    },
    [libraryId, effectiveSort, wallOrderParam, wallIndex, wallFilter, wallTop],
  );

  /**
   * 图廊里点心：收藏 / 取消收藏整部作品（与详情页那颗心同一落点）。
   */
  const toggleGalleryFavorite = useCallback(
    async (mediaItemId: string, next: boolean) => {
      const patch = (value: boolean) =>
        setGalleryGroups((current) =>
          current.map((g) =>
            g.media_item_id === mediaItemId ? { ...g, is_favorite: value } : g,
          ),
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

  const resetGallery = useCallback(() => {
    galleryGeneration.current += 1;
    galleryLoading.current = false;
    galleryCursors.current = [null];
    galleryLoaded.current = 0;
    setGalleryOrigin(0);
    setGalleryGroups([]);
    setGalleryHasMore(false);
    loadMoreGallery();
  }, [loadMoreGallery]);

  // 当前这份图廊窗口属于哪个库：快照带回来的窗口一进来就算数
  const galleryWindowLibrary = useRef<string | null>(
    snapshot?.galleryGroups.length ? libraryId : null,
  );

  useEffect(() => {
    // 偏好还没从 storage 读出来时先按兵不动：这一帧的排序是默认值，
    // 照它对账会把窗口按错的顺序重排一遍（见 useWallSortPref）
    if (!gallery || !wallSortReady) return;
    const sortChanged =
      galleryWindowSort.current !== null && galleryWindowSort.current !== gallerySortKey;
    galleryWindowSort.current = gallerySortKey;
    if (!sortChanged && galleryWindowLibrary.current === libraryId) {
      refreshGallery();
      return;
    }
    galleryWindowLibrary.current = libraryId;
    resetGallery();
  }, [gallery, libraryId, gallerySortKey, wallSortReady, resetGallery, refreshGallery]);

  return {
    lightboxIndex,
    setLightboxIndex,
    closeLightbox,
    galleryGrouped,
    setGalleryGrouped,
    galleryGroups,
    galleryHasMore,
    galleryOrigin,
    galleryCursors,
    galleryWindowSort,
    galleryLoaded,
    galleryEntries,
    loadMoreGallery,
    jumpGalleryTo,
    toggleGalleryFavorite,
    resetGallery,
  };
}
