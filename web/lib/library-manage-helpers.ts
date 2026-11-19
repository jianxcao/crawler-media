import type { LibraryKind } from "./media-types.ts";

/** 媒体库列表页的分类展示顺序。 */
export const KIND_ORDER: LibraryKind[] = ["movie", "tv", "video"];

/** 指针落在目标行的上半还是下半：决定放到它之前还是之后。 */
export function computeDropPosition(
  clientY: number,
  rectTop: number,
  rectHeight: number,
): "before" | "after" {
  return clientY < rectTop + rectHeight / 2 ? "before" : "after";
}

/** 重复文件列表使用的字节大小格式化。 */
export function formatManageBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}
