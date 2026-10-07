import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

export interface MediaCacheItem {
  id: string;
  source: "tmdb" | "douban" | "bangumi" | "anilist";
  source_id: string;
  title: string;
  original_title?: string | null;
  kind: "movie" | "tv";
  year?: number | null;
  poster_url?: string | null;
  backdrop_url?: string | null;
  overview?: string | null;
  rating?: number | null;
  genres: string[];
  fetched_at: number;
  expires_at?: number | null;
  cached_keys: string[];
}

export async function listCatalogCache(): Promise<MediaCacheItem[]> {
  const res = await request<ApiEnvelope<MediaCacheItem[]> | MediaCacheItem[]>("/catalog/cache?view=aggregate");
  if (Array.isArray(res)) return res;
  if (res && Array.isArray((res as ApiEnvelope<MediaCacheItem[]>).data)) {
    return (res as ApiEnvelope<MediaCacheItem[]>).data;
  }
  return [];
}

export async function deleteMediaCache(
  source: string,
  cacheKeys: string[],
): Promise<{ deleted: boolean; cleared_count?: number }> {
  const results = await Promise.all(
    cacheKeys.map((cacheKey) =>
      unwrap(
        request<ApiEnvelope<{ deleted: boolean }>>(
          `/catalog/cache?source=${encodeURIComponent(source)}&cache_key=${encodeURIComponent(cacheKey)}`,
          { method: "DELETE" },
        ),
      ),
    ),
  );
  return {
    deleted: results.some((result) => result.deleted),
    cleared_count: results.filter((result) => result.deleted).length,
  };
}
