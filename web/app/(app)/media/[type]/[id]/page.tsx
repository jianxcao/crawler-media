import { useParams, useSearchParams } from "react-router-dom";

import { MediaDetailView } from "@/components/media-detail-view";
import type { MediaSource } from "@/lib/media-types";
import { NotFound } from "@/src/not-found";

/** 影片详情（/media/movie|tv/[id]）：词条信息 + 剧照 + 相似推荐。 */
export default function MediaDetailPage() {
  const { type = "", id = "" } = useParams();
  const [searchParams] = useSearchParams();
  const source = (searchParams.get("source") as MediaSource) || "tmdb";
  if (type !== "movie" && type !== "tv") return <NotFound />;
  // key 按影片切换强制重建：在详情页内点「相似推荐」跳详情时回到顶部、重拉数据
  return <MediaDetailView key={`${source}:${type}:${id}`} type={type} id={id} source={source} />;
}
