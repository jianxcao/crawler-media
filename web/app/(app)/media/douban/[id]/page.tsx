import { useParams } from "react-router-dom";

import { MediaDetailView } from "@/components/media-detail-view";

/** 豆瓣独立详情路由；视觉与 TMDB 详情页完全复用 MediaDetailView。 */
export default function DoubanMediaDetailPage() {
  const { id = "" } = useParams();
  return <MediaDetailView key={`douban:${id}`} id={id} source="douban" />;
}
