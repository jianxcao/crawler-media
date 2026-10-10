import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

/** 刮削与整理设置（后端 `metadata.scrape` 域；空字段 = 跟随默认）。 */
export interface ScrapeSetting {
  language_priority: string[];
  cert_country_priority: string[];
  poster_mode: string;
  poster_language_priority: string[];
  backdrop_language_priority: string[];
  poster_min_width: number | null;
  backdrop_min_width: number | null;
  poster_size: string;
  backdrop_size: string;
  still_size: string;
  naming_entry_dir: string;
  naming_movie_file: string;
  naming_season_dir: string;
  naming_episode_file: string;
  mirror_images: boolean | null;
  mirror_nfo: boolean | null;
  mirror_episode_thumbs: boolean | null;
  theintrodb_enabled?: boolean | null;
  theintrodb_api_key?: string | null;
  fanart_api_key?: string | null;
  fanart_language?: string[];
  /** 声纹识别单集采样时长（秒），空 = 跟随默认 180（3 分钟）。 */
  fingerprint_duration_secs?: number | null;
}

export interface EffectiveScrapeConfig {
  language_priority: string[];
  cert_country_priority: string[];
  poster_mode: string;
  poster_language_priority: string[];
  backdrop_language_priority: string[];
  poster_min_width: number;
  backdrop_min_width: number;
  poster_size: string;
  backdrop_size: string;
  still_size: string;
  naming_entry_dir: string;
  naming_movie_file: string;
  naming_season_dir: string;
  naming_episode_file: string;
  mirror_images: boolean;
  mirror_nfo: boolean;
  mirror_episode_thumbs: boolean;
  theintrodb_enabled: boolean;
  theintrodb_api_key: string | null;
  fanart_configured: boolean;
  fingerprint_duration_secs: number;
}

export interface ScrapeConfig {
  setting: ScrapeSetting;
  effective: EffectiveScrapeConfig;
}

export interface NamingPreview {
  movie: string;
  tv: string;
  movie_media_title: string;
  tv_media_title: string;
}

/** 默认设置：与后端默认值一致的展示常量（保存为空的字段 = 跟随默认）。 */
export const SCRAPE_DEFAULTS: ScrapeSetting = {
  language_priority: [],
  cert_country_priority: [],
  poster_mode: "",
  poster_language_priority: [],
  backdrop_language_priority: [],
  poster_min_width: null,
  backdrop_min_width: null,
  poster_size: "",
  backdrop_size: "",
  still_size: "",
  naming_entry_dir: "",
  naming_movie_file: "",
  naming_season_dir: "",
  naming_episode_file: "",
  mirror_images: null,
  mirror_nfo: null,
  mirror_episode_thumbs: null,
  fanart_api_key: null,
  fanart_language: [],
  fingerprint_duration_secs: null,
};

export async function getScrapeConfig(): Promise<ScrapeConfig> {
  const envelope = await request<ApiEnvelope<ScrapeConfig>>("/settings/scrape");
  return envelope.data;
}

export async function saveScrapeConfig(setting: ScrapeSetting): Promise<ScrapeConfig> {
  const envelope = await request<ApiEnvelope<ScrapeConfig>>("/settings/scrape", {
    method: "PUT",
    body: JSON.stringify({ setting }),
  });
  return envelope.data;
}

export async function previewScrapeNaming(): Promise<NamingPreview> {
  const envelope = await request<ApiEnvelope<NamingPreview>>("/settings/scrape/preview-naming", {
    method: "POST",
  });
  return envelope.data;
}

export interface LanguageCandidate {
  code: string;
  name: string;
  english_name: string;
}

/** TMDB configuration 语种全表（设置页「更多语言」搜索）。 */
export async function fetchLanguageCandidates(): Promise<LanguageCandidate[]> {
  const envelope = await request<ApiEnvelope<{ languages: LanguageCandidate[] }>>(
    "/settings/languages",
  );
  return envelope.data?.languages ?? [];
}

export interface CountryCandidate {
  code: string;
  name: string;
  english_name: string;
}

/** TMDB configuration 地区全表（设置页「更多地区」搜索）。 */
export async function fetchCountryCandidates(): Promise<CountryCandidate[]> {
  const envelope = await request<ApiEnvelope<{ countries: CountryCandidate[] }>>(
    "/settings/countries",
  );
  return envelope.data?.countries ?? [];
}
