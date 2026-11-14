/**
 * 文件分页的唯一消费逻辑：把「首详情页 + 后续 `?after=` 页」走完，校验不重不漏。
 *
 * 这里不依赖任何 HTTP 客户端，调用方把「取下一页」注入进来，因此可以在
 * node 里用真实 DTO fixture 跑完整的 0/49/50/51/100/101 边界。
 */

export interface LibraryFilePageLike<T> {
  files: T[];
  next_cursor: string | null;
}

export interface PageFetcher<T> {
  (after: string): Promise<LibraryFilePageLike<T>>;
}

/**
 * 走完全部页。重复 `file_id` 会抛错（后端分页错误必须显式失败，
 * 不能被 UI 静默去重掩盖）；`next_cursor` 为 null 才是真正的末页。
 */
export async function collectAllFilePages<T extends { file_id: string }>(
  firstPage: LibraryFilePageLike<T>,
  fetchAfter?: PageFetcher<T>,
): Promise<T[]> {
  const files: T[] = [];
  const seen = new Set<string>();
  let page = firstPage;
  while (true) {
    for (const file of page.files) {
      if (seen.has(file.file_id)) {
        throw new Error(`文件分页出现重复 file_id: ${file.file_id}`);
      }
      seen.add(file.file_id);
      files.push(file);
    }
    const cursor = page.next_cursor;
    if (!cursor) return files;
    if (!fetchAfter) {
      throw new Error("分页未结束但缺少下一页取数函数");
    }
    page = await fetchAfter(cursor);
  }
}

/**
 * 只有已发布且在位的文件才是可播版本。缺失/未知在位或未发布（含缺字段）的
 * 文件可以查看，但不能被选来播放：不对旧响应形状 fail-open。
 */
export function isPlayableFile(file: {
  presence?: string | null;
  publication?: string | null;
}): boolean {
  return file.publication === "published" && file.presence === "present";
}

/** Only recorded coverage establishes which logical units a physical file serves. */
export function fileUnits(file: {
  units: Array<{ season: number; episode: number }>;
}): Array<{ season: number; episode: number }> {
  return file.units;
}

/** 该文件是否服务某一集（按完整覆盖判断，而非只认首个 unit）。 */
export function fileServesEpisode(
  file: Parameters<typeof fileUnits>[0],
  season: number,
  episode: number,
): boolean {
  return fileUnits(file).some((unit) => unit.season === season && unit.episode === episode);
}
