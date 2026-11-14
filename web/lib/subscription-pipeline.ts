export interface LibraryPipelineIdentityInput {
  name: string;
  kind: "movie" | "tv";
  is_default?: boolean;
}

/** 统一链路体检中媒体库名称、类别和默认库标记的展示数据。 */
export function libraryPipelineIdentity(pipeline: LibraryPipelineIdentityInput) {
  const name = pipeline.name.trim() || "未命名媒体库";
  return {
    name,
    kindLabel: pipeline.kind === "movie" ? "电影" : "剧集",
    isDefault: pipeline.is_default === true,
  };
}
