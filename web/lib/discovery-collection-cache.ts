/**
 * 发现片单请求模式：
 * - "preview": 发现主页横滚行/Hero 粗略预览，第一页可直接命中主页预填的 popularCache；
 * - "full": 完整片单落地页（「看全部」），第一页必须向服务端发起分页查询，以获取真实的分页元数据（total_pages / total_results / has_more）。
 */
export type DiscoveryCollectionMode = "preview" | "full";

export function shouldUsePreviewCache(
  mode: DiscoveryCollectionMode,
  page: number,
): boolean {
  return mode === "preview" && page === 1;
}
