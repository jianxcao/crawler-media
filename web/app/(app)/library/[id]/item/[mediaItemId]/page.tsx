import { useParams, useSearchParams } from "react-router-dom";

import { LibraryItemDetailView } from "@/components/library-item-detail-view";

/** 只接受单个非负整数查询参数；重复、负数和非数字一律按缺失处理。 */
function queryNumber(value: string | null): number | undefined {
  if (value == null || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

/**
 * 媒体库条目详情页（/library/[id]/item/[mediaItemId]）：
 * 展示"我拥有的这份拷贝"——本地刮削元数据 + 文件本体的真实介质规格，
 * 与发现页详情（/media/...，纯 TMDB 实时数据）是两个不同的页面。
 */
export default function LibraryItemDetailPage() {
  const { id = "", mediaItemId = "" } = useParams();
  const [searchParams] = useSearchParams();
  // 重复参数不产生歧义的返回目标，按缺失参数处理并回退到媒体库父级。
  const returnTo = searchParams.get("returnTo") ?? undefined;
  return (
    <div className="flex h-full flex-col">
      <LibraryItemDetailView
        libraryId={id}
        mediaItemId={mediaItemId}
        returnTo={returnTo}
        fromRecent={searchParams.get("from") === "recent"}
        initialSeason={queryNumber(searchParams.get("season"))}
        initialEpisode={queryNumber(searchParams.get("episode"))}
      />
    </div>
  );
}
