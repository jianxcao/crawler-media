import { useParams, useSearchParams } from "react-router-dom";

import { DiscoverView } from "@/components/discover-view";
import { discoveryFiltersKey, parseDiscoveryFilters } from "@/lib/discovery-filters";
import type { MediaSource } from "@/lib/media-types";
import { NotFound } from "@/src/not-found";

/** 发现页（/discover/movie | /discover/tv）：Hero 精选 + 分类横滚行。 */
export default function DiscoverPage() {
  const { type = "" } = useParams();
  const [searchParams] = useSearchParams();
  if (type !== "movie" && type !== "tv") return <NotFound />;
  // URL 是发现视角的唯一状态源；未知值安全回退到默认 TMDB 视角。
  const source: MediaSource = searchParams.get("source") === "douban" ? "douban" : "tmdb";
  const filters = parseDiscoveryFilters(Object.fromEntries(searchParams));
  return (
    <div className="flex h-full flex-col">
      <DiscoverView
        key={`${type}:${source}:${discoveryFiltersKey(filters)}`}
        mediaType={type}
        source={source}
        filters={filters}
        currentYear={new Date().getFullYear()}
      />
    </div>
  );
}
