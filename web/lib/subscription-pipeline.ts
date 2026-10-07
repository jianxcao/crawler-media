export interface LibraryPipelineIdentityInput {
  library_name?: string | null;
  /** automation-readiness 当前响应里的库名字段。 */
  name?: string | null;
  kind: "movie" | "tv";
  is_default?: boolean;
}

/** 统一链路体检中媒体库名称、类别和默认库标记的展示数据。 */
export function libraryPipelineIdentity(pipeline: LibraryPipelineIdentityInput) {
  const name = pipeline.library_name?.trim() || pipeline.name?.trim() || "未命名媒体库";
  return {
    name,
    kindLabel: pipeline.kind === "movie" ? "电影" : "剧集",
    isDefault: pipeline.is_default === true,
  };
}
