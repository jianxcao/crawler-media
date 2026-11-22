import { resolveRequestUrl } from "@/lib/http";
import { cachedImageUrl, type ImageVariant } from "@/lib/image-proxy";

/** 图片地址：本地美术图是 API 相对路径（补 base），TMDB 图床走缓存代理。 */
export function imageUrl(url: string | null, variant?: ImageVariant): string {
  if (!url) return "";
  if (/^https?:\/\//i.test(url)) {
    return cachedImageUrl(url, variant);
  }
  const withVariant =
    variant && /^\/?images\/assets\//.test(url)
      ? `${url}${url.includes("?") ? "&" : "?"}variant=${variant}`
      : url;
  return resolveRequestUrl ? (resolveRequestUrl(withVariant) ?? "") : withVariant;
}
