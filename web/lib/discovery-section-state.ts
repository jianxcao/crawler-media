export interface SectionErrorInfo {
  code: string;
  message: string;
}

/**
 * 校验并提取合法的分区错误对象。
 */
export function normalizeDiscoverySectionError(value: unknown): SectionErrorInfo | undefined {
  if (!value || typeof value !== "object") return undefined;
  const raw = value as { code?: unknown; message?: unknown };
  if (typeof raw.code === "string" && typeof raw.message === "string" && raw.code.trim() && raw.message.trim()) {
    return {
      code: raw.code.trim(),
      message: raw.message.trim(),
    };
  }
  return undefined;
}

/**
 * 判定分区是否允许进入成功片单集合缓存。带有错误的分区绝不缓存。
 */
export function shouldCacheDiscoverySection(error: SectionErrorInfo | undefined): boolean {
  return error === undefined;
}

/**
 * 判定发现页整体状态：
 * - "all-failed": 声明的所有分区均失败
 * - "partial": 部分分区失败，其余正常
 * - "ok": 全部正常
 */
export function discoverySectionStatus(
  sections: ReadonlyArray<{ error?: SectionErrorInfo }>,
): "all-failed" | "partial" | "ok" {
  if (sections.length === 0) return "ok";
  const failedCount = sections.filter((s) => s.error !== undefined).length;
  if (failedCount === sections.length) return "all-failed";
  if (failedCount > 0) return "partial";
  return "ok";
}

/**
 * 提取分区错误提示文案。正常空分区返回 null。
 */
export function discoverySectionMessage(section: { error?: SectionErrorInfo }): string | null {
  if (!section.error) return null;
  return section.error.message || "分区暂时无法加载，请重试";
}

/**
 * 提取 Hero 分区错误文案。若 Hero 分区存在错误返回可读提示，否则返回 null。
 */
export function heroFailureMessage(section: { error?: SectionErrorInfo }): string | null {
  return discoverySectionMessage(section);
}

/**
 * 汇总发现页重试时需要从前端内存缓存中清除的 key：
 * - pageKey: 当前页面 cacheKey
 * - collectionKeys: 之前加载失败的分区集合 key（成功且无错的分区不强清）
 */
export function retryCollectionCacheKeys(
  cacheKey: string,
  sections: ReadonlyArray<{ collectionRef: string; previewLimit: number; error?: SectionErrorInfo }>,
): { pageKey: string; collectionKeys: string[] } {
  const collectionKeys = sections
    .filter((s) => s.error !== undefined)
    .map((s) => `${s.collectionRef}:${s.previewLimit}`);
  return {
    pageKey: cacheKey,
    collectionKeys,
  };
}
