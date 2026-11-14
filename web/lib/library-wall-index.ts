/**
 * 海报墙 A-Z 索引条的几何：档位 ↔ 整份排序里的绝对位置。
 *
 * Task 26 起线上只传 `{initial, count, cursor}`：**绝对下标不进接口**，档位位置由
 * `count` 前缀和还原。跳转也不再按位置逐页走——每档自带一个不透明游标，点一个
 * 字母就是一次 `/items` 请求（见 `library-detail-view.tsx` 的 `jumpTo`）。
 */

interface WallIndexEntry {
  initial: string;
  /** 这一档有多少部作品 */
  count: number;
}

interface WallInitialMarker {
  initial: string;
  top: number;
}

/** 每档第一格在整份排序里的绝对位置（`count` 的前缀和）。 */
export function wallBucketOffsets(index: readonly WallIndexEntry[]): number[] {
  const offsets: number[] = [];
  let running = 0;
  for (const entry of index) {
    offsets.push(running);
    running += Math.max(0, entry.count);
  }
  return offsets;
}

/**
 * 绝对位置 `offset` 落在第几档；空索引或 `offset < 0` 返回 -1。
 *
 * 游标只到「这一档的第一格」，所以调用方拿这个下标取该档的游标后，还要按
 * `offset - 档起点` 决定是否在档内继续走页。
 */
export function wallBucketAtOffset(
  index: readonly WallIndexEntry[],
  offset: number,
): number {
  if (offset < 0) return -1;
  const offsets = wallBucketOffsets(index);
  let found = -1;
  for (let bucket = 0; bucket < offsets.length; bucket += 1) {
    if (offsets[bucket] <= offset) found = bucket;
    else break;
  }
  return found;
}

/** 整份拼音排序中的绝对位置 → 所属字母档。 */
export function wallInitialAtOffset(
  index: readonly WallIndexEntry[],
  offset: number,
): string | null {
  const bucket = wallBucketAtOffset(index, offset);
  return bucket < 0 ? null : index[bucket].initial;
}

/**
 * 字母首部影片的视口位置 → 当前活动字母。
 *
 * 锚点按海报 DOM 顺序传入；最后一个越过滚动容器顶边的锚点就是当前档。
 * 尚未滚到首个锚点时保留分页窗口所属档，避免索引在库头部区域突然无选中。
 */
export function activeWallInitialAtViewport(
  markers: readonly WallInitialMarker[],
  viewportTop: number,
  fallback: string | null,
): string | null {
  let active = fallback;
  for (const marker of markers) {
    if (marker.top > viewportTop) break;
    active = marker.initial;
  }
  return active;
}
