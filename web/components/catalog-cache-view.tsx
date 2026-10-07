"use client";

import { useEffect, useMemo, useState, useTransition } from "react";
import {
  deleteMediaCache,
  listCatalogCache,
  type MediaCacheItem,
} from "@/lib/api/catalog-cache";
import { imageUrl } from "@/lib/image-proxy";

const SOURCE_TABS = [
  { id: "all", label: "全部数据源" },
  { id: "tmdb", label: "TMDB (影视)" },
  { id: "douban", label: "豆瓣" },
  { id: "bangumi", label: "Bangumi (番组计划)" },
  { id: "anilist", label: "AniList (欧美动漫)" },
];

const KIND_TABS = [
  { id: "all", label: "全部作品" },
  { id: "movie", label: "电影" },
  { id: "tv", label: "电视剧" },
];

const PAGE_SIZE = 18;

export function CatalogCacheView() {
  const [items, setItems] = useState<MediaCacheItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedSource, setSelectedSource] = useState("all");
  const [selectedKind, setSelectedKind] = useState("all");
  const [searchTerm, setSearchTerm] = useState("");
  const [currentPage, setCurrentPage] = useState(1);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [isPending, startTransition] = useTransition();

  const loadData = async () => {
    try {
      setLoading(true);
      const data = await listCatalogCache();
      setItems(data);
      setError(null);
    } catch (e) {
      setError((e as Error).message || "加载元数据缓存失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const handleDelete = (item: MediaCacheItem) => {
    const keyCount = item.cached_keys?.length ?? 1;
    if (
      !window.confirm(
        `确定要清除《${item.title}》的本地元数据缓存吗？\n将清除包括基础信息、海报与剧照等 ${keyCount} 项缓存。`,
      )
    ) {
      return;
    }
    startTransition(async () => {
      try {
        setDeletingId(item.id);
        await deleteMediaCache(item.source, item.cached_keys);
        await loadData();
      } catch (e) {
        alert("清除缓存失败: " + (e as Error).message);
      } finally {
        setDeletingId(null);
      }
    });
  };

  const filtered = useMemo(() => {
    const list = Array.isArray(items) ? items : [];
    return list.filter((item) => {
      if (!item || !item.title || item.title.trim() === "" || item.title.trim() === "-") return false;
      if (selectedSource !== "all" && item.source !== selectedSource) return false;
      if (selectedKind !== "all" && item.kind !== selectedKind) return false;
      if (searchTerm) {
        const term = searchTerm.toLowerCase();
        const matchTitle = item.title.toLowerCase().includes(term);
        const matchOrig = item.original_title?.toLowerCase().includes(term) ?? false;
        const matchOverview = item.overview?.toLowerCase().includes(term) ?? false;
        const matchGenre = Array.isArray(item.genres) && item.genres.some((g) => g && g.toLowerCase().includes(term));
        if (!matchTitle && !matchOrig && !matchOverview && !matchGenre) return false;
      }
      return true;
    });
  }, [items, selectedSource, selectedKind, searchTerm]);

  useEffect(() => {
    setCurrentPage(1);
  }, [selectedSource, selectedKind, searchTerm]);

  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const pageItems = useMemo(() => {
    const start = (currentPage - 1) * PAGE_SIZE;
    return filtered.slice(start, start + PAGE_SIZE);
  }, [filtered, currentPage]);

  const tmdbCount = Array.isArray(items) ? items.filter((i) => i && i.source === "tmdb").length : 0;
  const doubanCount = Array.isArray(items) ? items.filter((i) => i && i.source === "douban").length : 0;
  const bangumiCount = Array.isArray(items) ? items.filter((i) => i && i.source === "bangumi").length : 0;
  const anilistCount = Array.isArray(items) ? items.filter((i) => i && i.source === "anilist").length : 0;

  return (
    <div className="mx-auto max-w-7xl px-4 py-8 pb-32 text-white">
      {/* 头部标题与操作 */}
      <div className="flex flex-wrap items-center justify-between gap-4 border-b border-white/10 pb-6">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">元数据缓存看板</h1>
          <p className="mt-1 text-sm text-[var(--text-muted)]">
            查看本地已缓存的电影与电视剧条目、高清封面及元数据详情，支持一键清退并重新拉取。
          </p>
        </div>
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={loadData}
            disabled={loading}
            className="rounded-lg bg-white/10 px-3.5 py-1.5 text-sm font-medium text-white transition hover:bg-white/15 disabled:opacity-50"
          >
            {loading ? "正在刷新..." : "刷新"}
          </button>
        </div>
      </div>

      {/* 概览统计卡片 */}
      <div className="mt-6 grid grid-cols-2 gap-3 sm:grid-cols-5">
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
          <div className="text-xs text-[var(--text-muted)]">已缓存总数</div>
          <div className="mt-1 text-xl font-bold">{filtered.length} 部</div>
        </div>
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
          <div className="text-xs text-[var(--text-muted)]">TMDB 影视</div>
          <div className="mt-1 text-xl font-bold text-sky-400">{tmdbCount} 部</div>
        </div>
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
          <div className="text-xs text-[var(--text-muted)]">豆瓣影视</div>
          <div className="mt-1 text-xl font-bold text-emerald-400">{doubanCount} 部</div>
        </div>
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
          <div className="text-xs text-[var(--text-muted)]">Bangumi 番剧</div>
          <div className="mt-1 text-xl font-bold text-pink-400">{bangumiCount} 部</div>
        </div>
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
          <div className="text-xs text-[var(--text-muted)]">AniList 动漫</div>
          <div className="mt-1 text-xl font-bold text-purple-400">{anilistCount} 部</div>
        </div>
      </div>

      {/* 过滤筛选栏 */}
      <div className="mt-6 space-y-3">
        {/* 数据源分类切换 */}
        <div className="flex items-center gap-2 overflow-x-auto pb-1 scrollbar-none">
          <span className="text-xs font-medium text-[var(--text-muted)] shrink-0">来源:</span>
          {SOURCE_TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setSelectedSource(tab.id)}
              className={`shrink-0 rounded-full px-3 py-1 text-xs font-medium transition ${
                selectedSource === tab.id
                  ? "bg-sky-500/20 text-sky-300 border border-sky-500/30 font-semibold"
                  : "bg-white/5 text-white/60 hover:bg-white/10 hover:text-white"
              }`}
            >
              {tab.label}
            </button>
          ))}
        </div>

        {/* 影视类型分类与搜索 */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-1.5 overflow-x-auto pb-1 scrollbar-none">
            {KIND_TABS.map((tab) => (
              <button
                key={tab.id}
                type="button"
                onClick={() => setSelectedKind(tab.id)}
                className={`shrink-0 rounded-lg px-3 py-1.5 text-xs transition ${
                  selectedKind === tab.id
                    ? "bg-white/[0.14] font-semibold text-white"
                    : "text-white/60 hover:bg-white/5 hover:text-white"
                }`}
              >
                {tab.label}
              </button>
            ))}
          </div>

          <div className="flex items-center justify-between gap-3">
            <input
              type="text"
              value={searchTerm}
              onChange={(e) => setSearchTerm(e.target.value)}
              placeholder="按片名、原名、类型或简介搜索..."
              className="w-full sm:w-72 rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-white placeholder-white/40 focus:border-sky-500 focus:outline-none"
            />
            <span className="text-xs text-[var(--text-muted)] shrink-0">
              共 {filtered.length} 部
            </span>
          </div>
        </div>
      </div>

      {error && (
        <div className="mt-4 rounded-lg border border-red-500/20 bg-red-500/10 p-4 text-sm text-red-400">
          {error}
        </div>
      )}

      {/* 竖版海报墙列表（2:3 标准电影海报） */}
      {filtered.length === 0 && !loading ? (
        <div className="mt-16 text-center text-sm text-[var(--text-muted)]">
          没有找到匹配的影视缓存记录。当系统自动订阅、搜索或抓取影片时，将在此呈现。
        </div>
      ) : (
        <div className="mt-6 grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6">
          {pageItems.map((item) => {
            const isDeleting = deletingId === item.id;

            return (
              <div
                key={item.id}
                className="group relative flex flex-col overflow-hidden rounded-xl border border-white/10 bg-white/[0.02] transition hover:border-white/20 hover:bg-white/[0.04]"
              >
                {/* 竖版电影海报 (2:3 黄金比例) */}
                <div className="relative aspect-[2/3] w-full overflow-hidden bg-black/40">
                  {item.poster_url ? (
                    <img
                      src={imageUrl(item.poster_url, "poster-card")}
                      alt={item.title}
                      className="h-full w-full object-cover transition duration-300 group-hover:scale-105"
                      loading="lazy"
                    />
                  ) : (
                    <div className="flex h-full w-full items-center justify-center text-xs text-white/30">
                      暂无海报
                    </div>
                  )}

                  {/* 悬浮遮罩与操作按钮 */}
                  <div className="absolute inset-0 flex flex-col justify-between bg-gradient-to-t from-black/80 via-transparent to-black/60 p-2 opacity-0 transition group-hover:opacity-100">
                    <div className="flex justify-between items-start">
                      <span className="rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-bold uppercase text-sky-400 backdrop-blur-md">
                        {item.source}
                      </span>
                      <button
                        type="button"
                        onClick={() => handleDelete(item)}
                        disabled={isDeleting || isPending}
                        title="清除该条目缓存"
                        className="rounded bg-red-500/80 p-1 text-[10px] text-white hover:bg-red-500 disabled:opacity-50"
                      >
                        清除
                      </button>
                    </div>

                    <div className="text-[10px] text-white/80 line-clamp-3 leading-snug">
                      {item.overview || "暂无简介"}
                    </div>
                  </div>

                  {/* 角标 */}
                  <div className="absolute top-2 left-2 flex gap-1 group-hover:opacity-0 transition">
                    <span className="rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wider text-sky-400 backdrop-blur-md">
                      {item.source}
                    </span>
                    <span className="rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white/80 backdrop-blur-md">
                      {item.kind === "movie" ? "电影" : "剧集"}
                    </span>
                  </div>

                  {item.rating != null && item.rating > 0 && (
                    <div className="absolute bottom-2 right-2 rounded bg-amber-500/90 px-1.5 py-0.5 text-[10px] font-bold text-black backdrop-blur-md">
                      ★ {item.rating.toFixed(1)}
                    </div>
                  )}
                </div>

                {/* 片名与年份 */}
                <div className="p-2.5">
                  <h3 className="line-clamp-1 text-xs font-semibold text-white" title={item.title}>
                    {item.title}
                  </h3>
                  <div className="mt-1 flex items-center justify-between text-[11px] text-white/40">
                    <span>{item.year ? `${item.year} 年` : "年份未知"}</span>
                    <span>{item.cached_keys?.length ?? 1} 项缓存</span>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* 分页导航 */}
      {totalPages > 1 && (
        <div className="mt-8 flex flex-wrap items-center justify-between gap-4 border-t border-white/10 pt-6">
          <div className="text-xs text-[var(--text-muted)]">
            显示第 {(currentPage - 1) * PAGE_SIZE + 1} 至{" "}
            {Math.min(currentPage * PAGE_SIZE, filtered.length)} 部，共 {filtered.length} 部影视作品
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={currentPage <= 1}
              onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-white/10 disabled:opacity-30"
            >
              上一页
            </button>

            <span className="px-2 font-mono text-xs text-white/80">
              {currentPage} / {totalPages}
            </span>

            <button
              type="button"
              disabled={currentPage >= totalPages}
              onClick={() => setCurrentPage((p) => Math.min(totalPages, p + 1))}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-white/10 disabled:opacity-30"
            >
              下一页
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
