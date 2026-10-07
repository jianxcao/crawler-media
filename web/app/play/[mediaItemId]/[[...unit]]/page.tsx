import { useParams, useSearchParams } from "react-router-dom";

import { PlayerPage } from "@/components/player/player-page";
import { parseUnitSegment } from "@/lib/player/play-links";

/**
 * 网页播放页（docs/design/web-player.md §6.10）。
 *
 * 地址就是分享凭证，形态按「短、稳、可读」设计：
 *
 *     电影：  /play/127
 *     剧集：  /play/127/s01e03
 *     指定位置：/play/127/s01e03?t=1520   （秒；YouTube 同款语义）
 *
 * - 只带 media_item_id：它以 (kind, tmdb_id) 为锚、幂等复用，换文件版本、
 *   重扫库都不变；库自增 id 删库重建就断，所以**不进地址**，由服务端按
 *   成员可见性解析归属。
 * - 季集用 sXXeYY 路径段而不是查询参数：媒体行业的通用写法，一眼能读。
 * - 不带标题 slug：中文进 URL 会被百分号编码，粘出来比数字更难看，且标题
 *   随刮削更新会变——同一内容两个地址比不可读更糟。
 * - `returnTo` 一类导航状态**永不进地址**：那是分享者的上下文，不是内容
 *   标识（走 sessionStorage，见 lib/player/return-path.ts）。
 */

/** 只接受单个非负整数查询参数；重复、负数和非数字一律按缺失处理。 */
function queryNumber(value: string | null): number | undefined {
  if (value == null || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

export default function PlayPage() {
  const params = useParams();
  const mediaItemId = params.mediaItemId ?? "";
  const unitParam = params["*"] || params.unit;
  const [searchParams] = useSearchParams();
  const parsed = parseUnitSegment(unitParam);
  const startSeconds = queryNumber(searchParams.get("t"));
  const playerKey = `${mediaItemId}:${parsed?.season ?? 0}:${parsed?.episode ?? 0}`;
  return (
    <PlayerPage
      key={playerKey}
      mediaItemId={mediaItemId}
      season={parsed?.season}
      episode={parsed?.episode}
      startMsOverride={startSeconds !== undefined ? startSeconds * 1000 : undefined}
    />
  );
}
