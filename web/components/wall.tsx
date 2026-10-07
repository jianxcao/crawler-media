"use client";

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";

import type { ImageVariant } from "@/lib/image-proxy";
import { tileWindowRange } from "@/lib/wall-window";

/**
 * 通用媒体墙内核（虚拟化瓦片窗口 + 密度偏好）：海报墙 / 图廊 / 搜索墙共用。
 * 原 photo-wall.tsx 的照片库专用部分（PhotoWall / 时间刻度 / 瀑布流布局）已随
 * Photos 模块删除。
 *
 * 与海报墙的区别只有一件事：混合长宽比。海报墙的格子比例是"一库一种"（2:3 或
 * 16:9），CSS grid 就能排；照片的比例每张不同，得自己算位置：
 *
 *   - **比例在渲染前已知**（`primary_aspect` 由扫描入账时读的原图尺寸得来），
 *     每张的高度 = 列宽 / 比例，放进当前最短的一列——Pinterest 的原版算法。
 *     不等图片加载，布局零抖动，也就能用 content-visibility 做虚拟化；
 *   - **极端比例夹到 [0.5, 2]**：全景图与长截图不能撑爆一列，超出的部分
 *     object-cover 裁掉，角标提示「全景 / 长图」，全屏时看完整原图；
 *   - **按月分组**：照片库的第一心智是"什么时候拍的"，整库一条瀑布流会把顺序
 *     打散；每个月一段标题 + 一面墙，段内再走最短列。月份来自条目的
 *     `release_date`（EXIF 拍摄日），与服务端的月份索引同一口径；
 *   - 瓦片绝对定位 + transform，密度切换 / 窗口变宽时重排走 CSS 过渡；
 *   - **只挂视口附近的瓦片**（虚拟化，见 useTileWindow）：几千张照片的库里，
 *     整墙上墙意味着几千个 DOM 节点、几千张解码位图，滑到后面手机就撑不住了。
 *     位置在渲染前已经算好，段的高度是显式的，虚拟化不需要任何测量。
 *
 * 数据仍是分页追加的（与海报墙同一套 loadMore），布局对已加载部分是最终的：
 * 后追加的条目只会落在同月末尾或新的月份段里，前面的瓦片不动。
 */

export type PhotoWallDensity = "compact" | "standard" | "loose";

/**
 * 三档密度各自的目标列宽、最少列数与间距。
 *
 * 只按目标列宽算列数在窄窗口上会失效：600px 以下三档都落到"至少两列"，点了
 * 没有任何变化；桌面上也只差一列，看不出来。所以每档还各自定最少列数与间距：
 * 紧凑在手机上也是三列、间距 6px，宽松在手机上是单列大图、间距 18px——
 * 任何宽度下三档都是三种明显不同的画面。
 */
export interface DensitySpec {
  /** 目标列宽（px）：列数 = floor((容器宽 + 间距) / (目标列宽 + 间距)) */
  column: number;
  minColumns: number;
  gap: number;
  /** 瓦片取哪个规格的图：列宽 ≤230 CSS px 用 480px 的 wall-tile 派生图（2x 屏够用），
   *  宽松密度列宽更大，直接用 720px 的缩略图本体 */
  variant: ImageVariant | undefined;
}
export const DENSITY: Record<PhotoWallDensity, DensitySpec> = {
  // 间距是「一面墙」与「一堆卡片」的分界：留白一宽，视线就被格线切碎，
  // 沉浸感没了。三档各自砍掉约一半（用户反馈 2026-09-07），仍保持
  // 紧凑 < 标准 < 宽松的梯度
  compact: { column: 150, minColumns: 3, gap: 3, variant: "wall-tile" },
  standard: { column: 230, minColumns: 2, gap: 6, variant: "wall-tile" },
  loose: { column: 340, minColumns: 1, gap: 10, variant: undefined },
};

const DENSITY_STORAGE_KEY = "crawler-media.wall.density";

/** 读写密度偏好：只是浏览器内的便利设置，读不到就用标准 */
export function usePhotoWallDensity(): [PhotoWallDensity, (next: PhotoWallDensity) => void] {
  const [density, setDensity] = useState<PhotoWallDensity>("standard");
  useEffect(() => {
    try {
      const stored = window.localStorage.getItem(DENSITY_STORAGE_KEY);
      if (stored === "compact" || stored === "standard" || stored === "loose") setDensity(stored);
    } catch {
      /* 隐私模式等拿不到 storage：保持默认 */
    }
  }, []);
  const update = useCallback((next: PhotoWallDensity) => {
    setDensity(next);
    try {
      window.localStorage.setItem(DENSITY_STORAGE_KEY, next);
    } catch {
      /* 同上 */
    }
  }, []);
  return [density, update];
}

interface Placement {
  x: number;
  y: number;
  width: number;
  height: number;
}

const GAP = 12;
const MIN_ASPECT = 0.5;
const MAX_ASPECT = 2;

/** 最短列放置：返回每张的位置与整面墙的高度。 */
export function layoutMasonry(
  aspects: readonly number[],
  containerWidth: number,
  targetColumnWidth: number,
  gap = GAP,
  minColumns = 2,
): { placements: Placement[]; height: number; columns: number } {
  const columns = Math.max(
    minColumns,
    Math.floor((containerWidth + gap) / (targetColumnWidth + gap)),
  );
  const columnWidth = (containerWidth - gap * (columns - 1)) / columns;
  const heights = new Array<number>(columns).fill(0);
  const placements = aspects.map((raw) => {
    const aspect = Math.min(MAX_ASPECT, Math.max(MIN_ASPECT, raw || 1));
    const height = columnWidth / aspect;
    let column = 0;
    for (let i = 1; i < columns; i += 1) {
      if (heights[i] < heights[column] - 0.5) column = i;
    }
    const placement = {
      x: column * (columnWidth + gap),
      y: heights[column],
      width: columnWidth,
      height,
    };
    heights[column] += height + gap;
    return placement;
  });
  const height = aspects.length === 0 ? 0 : Math.max(...heights) - gap;
  return { placements, height, columns };
}

/** 稀疏行：一行等高、限高（照片数少于列数时避免孤柱）。 */
export function layoutSparseRow(
  aspects: readonly number[],
  containerWidth: number,
  targetColumnWidth: number,
  gap = GAP,
): { placements: Placement[]; height: number } {
  const ratios = aspects.map((raw) => Math.min(MAX_ASPECT, Math.max(MIN_ASPECT, raw || 1)));
  const sum = ratios.reduce((a, b) => a + b, 0);
  const fitHeight = (containerWidth - gap * (ratios.length - 1)) / Math.max(sum, 0.01);
  const height = Math.min(targetColumnWidth * 1.2, fitHeight);
  let x = 0;
  const placements = ratios.map((ratio) => {
    const placement = { x, y: 0, width: ratio * height, height };
    x += ratio * height + gap;
    return placement;
  });
  return { placements, height: ratios.length === 0 ? 0 : height };
}

const OVERSCAN_SCREENS = 1.5;

/**
 * 一面墙上所有段共用的滚动广播。
 *
 * 每段各自监听 scroll 的话，几十段就是几十个监听器与几十次 rAF。这里合成一个：
 * 用**捕获阶段**监听 window 的 scroll——库页真正滚动的是内部容器，scroll 事件
 * 不冒泡，只有捕获收得到；这样也不必把滚动容器一路传进每一段。
 *
 * 广播时附带「这一帧滑了多少像素」。远处的段据此决定这次要不要真去量
 * （见 useTileWindow 的 budget）：十年的相册有两百多个月份段，每段每帧都读一次
 * 布局，光这一项就要吃掉三成帧预算。滑动距离是唯一能让段与视口的相对位置
 * 发生变化的东西，所以按它计费既省得准、又不会漏——一次跳转（回到上次位置、
 * 时间刻度跳月）会一次性把所有预算吃光，段立刻重新量。
 */
const wallWatchers = new Set<(movedPx: number) => void>();
let wallFrame = 0;
let lastScrollTop: number | null = null;
/** 本帧滑过的距离；量不准（resize、换了滚动容器）时给 Infinity＝所有段都重新量 */
let movedPx = Number.POSITIVE_INFINITY;

function onWallScroll(event: Event) {
  const target = event.target;
  const top =
    target instanceof Element
      ? target.scrollTop
      : (document.scrollingElement?.scrollTop ?? null);
  if (top === null) movedPx = Number.POSITIVE_INFINITY;
  else {
    movedPx = lastScrollTop === null ? Number.POSITIVE_INFINITY : Math.abs(top - lastScrollTop);
    lastScrollTop = top;
  }
  scheduleWallFrame();
}
function onWallResize() {
  // 视口尺寸变了，带子的边界跟着变，谁都不能再吃预算
  movedPx = Number.POSITIVE_INFINITY;
  lastScrollTop = null;
  scheduleWallFrame();
}
function scheduleWallFrame() {
  if (wallFrame) return;
  wallFrame = requestAnimationFrame(() => {
    wallFrame = 0;
    const moved = movedPx;
    movedPx = 0;
    for (const watcher of wallWatchers) watcher(moved);
  });
}
function subscribeWall(watcher: (movedPx: number) => void): () => void {
  if (wallWatchers.size === 0) {
    window.addEventListener("scroll", onWallScroll, { capture: true, passive: true });
    window.addEventListener("resize", onWallResize, { passive: true });
  }
  wallWatchers.add(watcher);
  return () => {
    wallWatchers.delete(watcher);
    if (wallWatchers.size > 0) return;
    window.removeEventListener("scroll", onWallScroll, { capture: true });
    window.removeEventListener("resize", onWallResize);
    if (wallFrame) cancelAnimationFrame(wallFrame);
    wallFrame = 0;
    lastScrollTop = null;
  };
}

/**
 * 立刻让墙上所有段重新量一次虚拟化窗口（同步，不等下一帧）。
 *
 * 给「代码自己改了 scrollTop」的场合用：scroll 事件要到下一帧才来，中间那一帧
 * 挂着的还是旧位置附近的瓦片，看起来就是闪一下空墙。库页向上补页时会把墙已
 * 加载的部分整体往下推、再把 scrollTop 加回去（见 library-detail-view 的前置
 * 加载补偿），补完调一次这里，这一帧就已经是新位置该挂的那几块。
 */
export function remeasureWalls(): void {
  // 复制一份再遍历：量测里会 setState，React 可能顺手让某个段卸载退订
  for (const watcher of [...wallWatchers]) watcher(Number.POSITIVE_INFINITY);
}

/**
 * 本段当前该挂哪一段瓦片：返回 [起, 止) 的下标区间。
 *
 * 一次量测 ＝ 一次 getBoundingClientRect（读段容器本身，不读瓦片里的 <img>
 * ——那个读法在 content-visibility 的格子里每读一次就逼一次全量布局）＋两次
 * 二分。区间没变就不 setState，因此绝大多数帧里整棵树一次重渲染都没有。
 *
 * 远处的段按**距离预算**跳过量测：量完一次就记下「离带子还有多远」，之后每帧
 * 扣掉滑过的距离，扣光了才重新量。这不是近似——滑动是段与视口相对位置变化的
 * 唯一来源，所以离带子 8000px 的段在页面又滑了 8000px 之前，不可能需要改窗口。
 * 十年的相册有两百多个月份段，每段每帧都读一次布局要吃掉三成帧预算（实测
 * 五万张时滚动主线程占用 57%），按预算跳过之后回到 35%，且不引入任何延迟。
 */
export function useTileWindow(
  containerRef: RefObject<HTMLElement | null>,
  placements: readonly Placement[],
): readonly [number, number] {
  const [range, setRange] = useState<readonly [number, number]>([0, 0]);
  // 当前区间也存一份在 ref 里，就为了「没变就一次 setState 都不发」。
  // 用 setRange(current => current) 是不够的：即便返回同一个值，React 也可能
  // 先把这个组件重渲一遍再决定跳过。一面墙上两百多段、每秒 60 帧，那是每秒
  // 上万次白跑的组件渲染——五万张时滚动的主线程占用有一半出在这里。
  const rangeRef = useRef(range);
  // 二分只能定位「y 不小于某值的第一块」，而跨在带子上沿的那块 y 更小。
  // 往回退一整块最高瓦片的高度，保证它也在区间里
  const tallest = useMemo(
    () => placements.reduce((max, p) => (p.height > max ? p.height : max), 0),
    [placements],
  );

  // layout effect：首帧就把窗口量出来，否则会先画一帧空段再补上瓦片，
  // 看起来像闪了一下（段容器只在容器宽度量到之后才渲染，不会跑在服务端）
  useLayoutEffect(() => {
    const commit = (next: readonly [number, number]) => {
      if (rangeRef.current[0] === next[0] && rangeRef.current[1] === next[1]) return;
      rangeRef.current = next;
      setRange(next);
    };
    // 还可以再让页面滑多少像素才需要重新量（见上）
    let budget = 0;
    const measure = (movedPx = Number.POSITIVE_INFINITY) => {
      budget -= movedPx;
      if (budget > 0) return;
      const el = containerRef.current;
      if (!el || placements.length === 0) {
        commit([0, 0]);
        return;
      }
      // 段顶相对视口的位置：负值表示段顶已经滑到视口上方
      const rect = el.getBoundingClientRect();
      const overscan = window.innerHeight * OVERSCAN_SCREENS;
      commit(tileWindowRange(placements, rect.top, window.innerHeight, overscan, tallest));
      // 本段离「视口 ± 提前量」这条带子还有多远：在带子里就是 0（下一帧照常量），
      // 在带子外就是下一次量测之前可以放心滑过的距离
      budget = Math.max(0, rect.top - (window.innerHeight + overscan), -overscan - rect.bottom);
    };
    measure();
    return subscribeWall(measure);
  }, [containerRef, placements, tallest]);

  return range;
}

/**
 * 悬浮时间刻度（Google Photos 式）：覆在墙的右缘、不占宽度，平时几乎不可见，
 * 滚动中或把鼠标移到右缘时浮现。
 *
 * 之前是一条常驻的月份列表放在墙旁边，占 56px 宽还一直亮着，浏览照片时是个
 * 干扰。这里换成零宽的占位列 + sticky 的刻度条向左负边距叠在墙上：墙用满整个
 * 宽度，刻度只在需要时出现。刻度按每月张数**按比例**分布（张数多的月份占的
 * 刻度段长），年份标签立在该年第一个月处；悬停某段浮出「2026 年 8 月 · 16 张」
 * 气泡，点击跳到该月第一张；当前所在月份常亮。
 */
