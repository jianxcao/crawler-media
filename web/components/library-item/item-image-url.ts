import { resolveRequestUrl } from "@/lib/http";
import { cachedImageUrl } from "@/lib/image-proxy";

/** 图片地址：本地美术图是 API 相对路径（补 base），TMDB 图床走缓存代理。 */
export function imageUrl(url: string | null): string {
  if (!url) return "";
  return /^https?:\/\//i.test(url)
    ? cachedImageUrl(url)
    : resolveRequestUrl
      ? (resolveRequestUrl(url) ?? "")
      : url;
}
