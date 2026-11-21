"use client";

import { useEffect, useState } from "react";

import {
  listLibraryRoutingOptions,
  type MatchRule,
  type RoutingOptions,
} from "@/lib/api/libraries";
import type { LibraryKind } from "@/lib/media-types";

/**
 * 收藏范围可选项的模块级缓存：这是后端静态常量，但 useRoutingOptions 被
 * 每张库卡片和表单弹窗各自调用——不缓存的话库首页一次挂载就打出 N 个
 * 相同请求。失败时清空缓存，下一个调用方重试。
 */
let routingOptionsPromise: Promise<RoutingOptions> | null = null;

/** 收藏范围可选项（后端静态常量）；加载失败降级为 null（相关 UI 不渲染）。 */
export function useRoutingOptions(): RoutingOptions | null {
  const [options, setOptions] = useState<RoutingOptions | null>(null);
  useEffect(() => {
    let cancelled = false;
    routingOptionsPromise ??= listLibraryRoutingOptions();
    void routingOptionsPromise
      .then((o) => {
        if (!cancelled) setOptions(o);
      })
      .catch(() => {
        /* 静默降级：收藏范围区显示加载失败提示 */
        routingOptionsPromise = null;
      });
    return () => {
      cancelled = true;
    };
  }, []);
  return options;
}

/** 收藏范围条件 → 表单状态（v1 两个维度：类型 ID / 区域国家码）。 */
export function parseMatchRules(rules: MatchRule[]): { genres: number[]; regions: string[] } {
  const genres = rules.find((r) => r.field === "genres")?.values ?? [];
  const regions = rules.find((r) => r.field === "origin_countries")?.values ?? [];
  return {
    genres: genres.filter((v): v is number => typeof v === "number"),
    regions: regions.filter((v): v is string => typeof v === "string"),
  };
}

/** 表单状态 → 收藏范围条件（空维度不生成条件；两者都空 = 不声明）。 */
export function buildMatchRules(genres: number[], regions: string[]): MatchRule[] {
  const rules: MatchRule[] = [];
  if (genres.length > 0) rules.push({ field: "genres", op: "any_of", values: genres });
  if (regions.length > 0) {
    rules.push({ field: "origin_countries", op: "any_of", values: regions });
  }
  return rules;
}

/**
 * 把区域国家码折叠成展示名：整组命中的折叠成预设组名（如「日韩」），
 * 折不进组的逐个显示中文名。用于表单弹窗底栏的当前声明摘要。
 */
export function regionLabels(regions: string[], options: RoutingOptions): string[] {
  const parts: string[] = [];
  let rest = [...regions];
  for (const preset of options.region_presets) {
    if (preset.countries.every((c) => rest.includes(c))) {
      parts.push(preset.label);
      rest = rest.filter((c) => !preset.countries.includes(c));
    }
  }
  parts.push(...rest.map((c) => options.country_names[c] ?? c));
  return parts;
}

export const INPUT_CLASS =
  "w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui " +
  "text-[var(--text)] outline-none focus:border-[var(--accent)]/60";
export const LABEL_CLASS = "mb-1.5 block text-sub font-medium text-[var(--text-muted)]";

/** 类型卡片的文案：讲清后果，不提识别用的是哪个数据源；「其他」放什么由用户定。 */
export const KIND_CARDS: Record<
  LibraryKind,
  { blurb: string; traits: string; chosen: string; defaultName: string; defaults: string }
> = {
  movie: {
    blurb: "自动识别影片，补齐简介、评分、演职员与海报剧照；一部片一个目录。",
    traits: "可订阅 · 可整理文件名",
    chosen: "自动识别并补齐元数据与图片",
    defaultName: "电影库",
    defaults:
      "实时监控目录变化、扫描后保留丢失记录、为未识别文件生成封面、在首页展示。" +
      "第一个电影库自动成为默认库；刮削偏好建好后在「编辑库」里设。",
  },
  tv: {
    blurb: "自动识别剧集与季集，补齐分集信息与图片；追新订阅按集补齐。",
    traits: "可订阅 · 可整理文件名",
    chosen: "自动识别季集并补齐元数据与图片",
    defaultName: "剧集库",
    defaults:
      "实时监控目录变化、扫描后保留丢失记录、为未识别文件生成封面、在首页展示。" +
      "第一个剧集库自动成为默认库；刮削偏好建好后在「编辑库」里设。",
  },
  video: {
    blurb:
      "不识别、不刮削、不改名的视频：放什么由你定。有 NFO 就读 NFO，否则按文件名展示，封面从视频里抓帧。",
    traits: "不识别 · 可播放 · 记进度",
    chosen: "不识别不刮削，按 NFO / 文件名展示",
    defaultName: "其他",
    defaults:
      "实时监控目录变化、扫描后保留丢失记录、从视频抓帧生成封面、在首页展示。" +
      "网络挂载的目录建议建好后到「编辑库 → 扫描与监控」关掉实时监控与抓帧。",
  },
};

/** 新建时的类型是否带识别链（决定有没有收藏范围这一步）；已有库读服务端能力位。 */
export function kindIsScraped(kind: LibraryKind): boolean {
  return kind !== "video";
}

/** 新建时「在首页展示」的默认值。 */
export function kindExcludedFromHome(): boolean {
  return false;
}

/** 去掉尾部斜杠（保留根 `/`）：`/media/` 与 `/media` 是同一个目录，不该触发换根。 */
export function normalizeRoots(roots: string[]): string[] {
  return roots.map((root) => {
    const trimmed = root.trim().replace(/\/+$/, "");
    return trimmed === "" ? "/" : trimmed;
  });
}

/** 换根与否只比较规范化后的路径列表，顺序即优先级，顺序变了也是换根。 */
export function sameRoots(left: string[], right: string[]): boolean {
  const normalized = normalizeRoots(right);
  return left.length === normalized.length && left.every((root, index) => root === normalized[index]);
}

/** 收藏范围的一句话摘要（新建第 3 步底部与编辑分区的折叠行共用）。 */
export function scopeSummary(
  kind: LibraryKind,
  regions: string[],
  genres: number[],
  options: RoutingOptions | null,
): { declared: boolean; text: string } {
  if (options === null) {
    const declared = regions.length > 0 || genres.length > 0;
    return { declared, text: declared ? "已声明收藏范围" : "未声明" };
  }
  const genreOptions = kind === "movie" ? options.movie_genres : options.tv_genres;
  const parts = [
    regionLabels(regions, options).join(" / "),
    genreOptions
      .filter((g) => genres.includes(g.id))
      .map((g) => g.label)
      .join(" / "),
  ].filter(Boolean);
  return parts.length > 0
    ? { declared: true, text: parts.join(" 的 ") }
    : { declared: false, text: "未声明" };
}

/** 类型 ID 只保留当前库类型下有效的（切换过类型时另一类型独有的 ID 不带进声明）。 */
export function validGenres(
  kind: LibraryKind,
  genres: number[],
  options: RoutingOptions | null,
): number[] {
  if (options === null) return genres;
  const valid = new Set(
    (kind === "movie" ? options.movie_genres : options.tv_genres).map((g) => g.id),
  );
  return genres.filter((id) => valid.has(id));
}
