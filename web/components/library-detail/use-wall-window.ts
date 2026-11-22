"use client";

import { useCallback, useRef, type Dispatch, type MutableRefObject, type SetStateAction } from "react";

import {
  listLibraryItemIndex,
  listLibraryItems,
  type LibraryFilter,
  type LibraryIndexEntry,
  type LibraryItem,
  type LibraryItemOrder,
  type LibraryItemSort,
} from "@/lib/api/libraries";
import { HttpError } from "@/lib/http";
import { hasMorePages, INDEXED_SORTS, WALL_API_PAGE_SIZE, WALL_PAGE_SIZE } from "@/lib/library-detail-helpers";
import { wallBucketAtOffset, wallBucketOffsets } from "@/lib/library-wall-index";
import { keepIfEqual, reconcileList } from "@/lib/poll-reconcile";

export function useWallWindow({
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
}: {
  libraryId: string;
  reloadSeq: MutableRefObject<number>;
  wallSort: MutableRefObject<LibraryItemSort>;
  wallOrder: MutableRefObject<LibraryItemOrder | undefined>;
  wallFilter: MutableRefObject<LibraryFilter>;
  wallCursors: MutableRefObject<(string | null)[]>;
  wallLoaded: MutableRefObject<number>;
  galleryRef: MutableRefObject<boolean>;
  lastIndexGalleryRef: MutableRefObject<boolean>;
  wallIndex: LibraryIndexEntry[];
  pendingJump: MutableRefObject<number | null>;
  setItems: Dispatch<SetStateAction<LibraryItem[]>>;
  setWallOrigin: Dispatch<SetStateAction<number>>;
  setWallHasMore: Dispatch<SetStateAction<boolean>>;
  setWallIndex: Dispatch<SetStateAction<LibraryIndexEntry[]>>;
  setFailed: Dispatch<SetStateAction<boolean>>;
}) {
  const loadingMore = useRef(false);

  const loadWallPages = useCallback(
    async (pages: number, seq: number): Promise<LibraryItem[]> => {
      const cursors: (string | null)[] = [wallCursors.current[0] ?? null];
      const query = {
        sort: wallSort.current,
        order: wallOrder.current,
        filter: wallFilter.current,
        limit: WALL_API_PAGE_SIZE,
      };
      const fetchAt = (page: number, cursor: string | null) =>
        listLibraryItems(libraryId, {
          ...query,
          cursor,
        }).then((result) => {
          cursors[page] = cursor;
          cursors[page + 1] = result.next_cursor;
          return result.items;
        });
      const prefix: LibraryItem[] = [];
      for (let page = 0; page < pages; page += 1) {
        if (seq !== reloadSeq.current) return [];
        if (page > 0 && cursors[page] === null) break;
        prefix.push(...(await fetchAt(page, cursors[page] ?? null)));
      }
      if (seq !== reloadSeq.current) return [];
      const end = cursors.indexOf(null, 1);
      cursors.length = end < 0 ? Math.min(cursors.length, pages + 1) : end + 1;
      wallCursors.current = cursors;
      return prefix;
    },
    [libraryId, reloadSeq, wallCursors, wallFilter, wallOrder, wallSort],
  );

  const fetchWall = useCallback(
    (seq: number) => {
      const pages = Math.max(1, Math.ceil(wallLoaded.current / WALL_API_PAGE_SIZE));
      const indexKind = INDEXED_SORTS[wallSort.current];
      const currentFilter = wallFilter.current;
      const order = wallOrder.current;
      const galleryMode = galleryRef.current;
      lastIndexGalleryRef.current = galleryMode;
      const fetchIndex = (): Promise<LibraryIndexEntry[]> =>
        indexKind === undefined
          ? Promise.resolve([])
          : listLibraryItemIndex(libraryId, indexKind, currentFilter, order, {
              gallery: galleryMode,
            }).catch(() => []);
      return loadWallPages(pages, seq)
        .catch((e) => {
          if (e instanceof HttpError && e.status === 404) return [] as LibraryItem[];
          throw e;
        })
        .then((libraryItems) =>
          (seq === reloadSeq.current ? fetchIndex() : Promise.resolve([]))
            .then((index) => [libraryItems, index] as const),
        );
    },
    [galleryRef, lastIndexGalleryRef, libraryId, loadWallPages, reloadSeq, wallFilter, wallLoaded, wallOrder, wallSort],
  );

  const reloadWall = useCallback(() => {
    const seq = ++reloadSeq.current;
    fetchWall(seq)
      .then(([libraryItems, index]) => {
        if (seq !== reloadSeq.current) return;
        setItems((prev) => reconcileList(prev, libraryItems, (i) => i.media_item_id));
        wallLoaded.current = Math.max(WALL_PAGE_SIZE, libraryItems.length);
        setWallHasMore(hasMorePages(wallCursors.current));
        setWallIndex((prev) => keepIfEqual(prev, index));
      })
      .catch(() => {
        if (seq === reloadSeq.current) setFailed(true);
      });
  }, [fetchWall, reloadSeq, setFailed, setItems, setWallHasMore, setWallIndex, wallCursors, wallLoaded]);

  const loadMore = useCallback(() => {
    if (loadingMore.current) return;
    const next = wallCursors.current.length - 1;
    const cursor = wallCursors.current[next] ?? null;
    if (next > 0 && cursor === null) {
      setWallHasMore(false);
      return;
    }
    loadingMore.current = true;
    const requestSeq = ++reloadSeq.current;
    listLibraryItems(libraryId, {
      sort: wallSort.current,
      order: wallOrder.current,
      filter: wallFilter.current,
      limit: WALL_API_PAGE_SIZE,
      cursor,
    })
      .then((result) => {
        if (requestSeq !== reloadSeq.current) return;
        wallCursors.current[next] = cursor;
        wallCursors.current[next + 1] = result.next_cursor;
        wallLoaded.current += result.items.length;
        setItems((current) => {
          const seen = new Set(current.map((i) => i.media_item_id));
          return [...current, ...result.items.filter((i) => !seen.has(i.media_item_id))];
        });
        setWallHasMore(result.next_cursor !== null);
      })
      .catch(() => {
        if (requestSeq === reloadSeq.current) setWallHasMore(false);
      })
      .finally(() => {
        loadingMore.current = false;
      });
  }, [libraryId, reloadSeq, setItems, setWallHasMore, wallCursors, wallFilter, wallLoaded, wallOrder, wallSort]);

  const jumpTo = useCallback(
    (offset: number) => {
      const target = Math.max(0, offset);
      if (loadingMore.current) return;
      const bucket = wallBucketAtOffset(wallIndex, target);
      const bucketStart = bucket < 0 ? 0 : wallBucketOffsets(wallIndex)[bucket];
      const cursor = bucket < 0 ? null : wallIndex[bucket].cursor;
      loadingMore.current = true;
      const seq = ++reloadSeq.current;
      const query = {
        sort: wallSort.current,
        order: wallOrder.current,
        filter: wallFilter.current,
        limit: WALL_API_PAGE_SIZE,
      };
      const fetchAt = (from: string | null) =>
        listLibraryItems(libraryId, {
          ...query,
          cursor: from,
        });
      void (async () => {
        const first = await fetchAt(cursor);
        let itemsInWindow = first.items;
        let next = first.next_cursor;
        const chain: (string | null)[] = [cursor];
        const seenCursors = new Set<string>();
        if (cursor !== null) seenCursors.add(cursor);
        if (first.items.length === 0 || (next !== null && seenCursors.has(next))) {
          next = null;
        }
        while (next !== null && target - bucketStart >= itemsInWindow.length) {
          if (seq !== reloadSeq.current) return;
          seenCursors.add(next);
          chain.push(next);
          const page = await fetchAt(next);
          itemsInWindow = [...itemsInWindow, ...page.items];
          const nextCursor = page.next_cursor;
          if (page.items.length === 0 || (nextCursor !== null && seenCursors.has(nextCursor))) {
            next = null;
            break;
          }
          next = nextCursor;
        }
        chain.push(next);
        if (seq !== reloadSeq.current) return;
        wallCursors.current = chain;
        setWallOrigin(bucketStart);
        wallLoaded.current = Math.max(WALL_PAGE_SIZE, itemsInWindow.length);
        setItems(itemsInWindow);
        setWallHasMore(next !== null);
        pendingJump.current = target - bucketStart;
      })()
        .catch(() => {})
        .finally(() => {
          loadingMore.current = false;
        });
    },
    [libraryId, pendingJump, reloadSeq, setItems, setWallHasMore, setWallOrigin, wallCursors, wallFilter, wallIndex, wallLoaded, wallOrder, wallSort],
  );

  return { loadingMore, fetchWall, reloadWall, loadMore, jumpTo };
}
