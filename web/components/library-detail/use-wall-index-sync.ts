"use client";

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";

import type { LibraryIndexEntry, LibraryItem } from "@/lib/api/libraries";
import { wallBucketOffsets, wallInitialAtOffset } from "@/lib/library-wall-index";
import { activeInitialAt } from "@/lib/wall-window";

export function useWallIndexSync({
  items,
  wallIndex,
  wallOrigin,
  scrollElement,
  probing,
  wallGrid,
}: {
  items: LibraryItem[];
  wallIndex: LibraryIndexEntry[];
  wallOrigin: number;
  scrollElement: HTMLDivElement | null;
  probing: boolean;
  wallGrid: RefObject<HTMLDivElement | null>;
}) {
  // 活动字母要跟随用户在已加载前缀里的真实滚动位置。
  const [activeWallInitial, setActiveWallInitial] = useState<string | null>(null);
  const wallGeometry = useRef<{ rowTops: readonly number[]; columns: number } | null>(null);
  /**
   * 跳转请求的目标绝对下标：前缀走到位后由下面的 layout effect 落到那一格。
   *
   * 不能请求一回来就滚——那时 React 还没把新条目铺上墙，几何与滚动高度都还是
   * 旧前缀的，滚到的位置一定是错的。
   */
  const pendingJump = useRef<number | null>(null);
  const onWallGeometry = useCallback(
    (geometry: { rowTops: readonly number[]; columns: number } | null) => {
      wallGeometry.current = geometry;
    },
    [],
  );

  // 各字母首部条目在**已加载窗口里**的下标：档位绝对位置（count 前缀和）减窗口起点
  const wallAnchors = useMemo(() => {
    const offsets = wallBucketOffsets(wallIndex);
    return wallIndex
      .map((entry, bucket) => ({ initial: entry.initial, index: offsets[bucket] - wallOrigin }))
      .filter((anchor) => anchor.index >= 0);
  }, [wallIndex, wallOrigin]);

  useLayoutEffect(() => {
    const target = pendingJump.current;
    if (target === null) return;
    const geometry = wallGeometry.current;
    const grid = wallGrid.current;
    if (!geometry || !grid || !scrollElement) return;
    const row = Math.floor(target / Math.max(1, geometry.columns));
    const rowTop = geometry.rowTops[row];
    if (rowTop === undefined) return; // 这次重排还没铺到那一行，等下一次 items 变化
    pendingJump.current = null;
    const offset =
      grid.getBoundingClientRect().top -
      scrollElement.getBoundingClientRect().top +
      scrollElement.scrollTop;
    // 瞬时而不是平滑：墙内容整段换了，平滑滚过去的是一堆不存在的旧内容
    scrollElement.scrollTo({ top: Math.max(0, offset + rowTop), behavior: "instant" });
  }, [items, scrollElement, wallGrid]);

  useEffect(() => {
    const fallback = wallInitialAtOffset(wallIndex, wallOrigin);
    setActiveWallInitial(fallback);
    if (!scrollElement || probing) return;

    let frame = 0;
    const update = () => {
      frame = 0;
      const grid = wallGrid.current;
      if (!grid) return;
      const viewportTop = scrollElement.getBoundingClientRect().top + 1;
      const geometry = wallGeometry.current;
      const next = geometry
        ? activeInitialAt(
            wallAnchors,
            geometry.rowTops,
            geometry.columns,
            viewportTop - grid.getBoundingClientRect().top,
            fallback,
          )
        : fallback;
      setActiveWallInitial((current) => (current === next ? current : next));
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };

    scrollElement.addEventListener("scroll", schedule, { passive: true });
    schedule();
    return () => {
      scrollElement.removeEventListener("scroll", schedule);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [items.length, probing, scrollElement, wallAnchors, wallGrid, wallIndex, wallOrigin]);

  return {
    onWallGeometry,
    pendingJump,
    activeWallInitial,
    setActiveWallInitial,
  };
}
