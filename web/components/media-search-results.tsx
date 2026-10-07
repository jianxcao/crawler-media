"use client";

import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";

import { PosterCardVisual } from "@/components/poster-card";
import type { MediaSearchItem } from "@/lib/api/discover";
import { getTitleSearchHistoryResults, searchTitles } from "@/lib/api/search";
import type { MediaSource } from "@/lib/media-types";
import { formatRelativeTime } from "@/lib/time";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

const MEDIA_SOURCES: { id: MediaSource; label: string }[] = [
  { id: "douban", label: "豆瓣" },
  { id: "tmdb", label: "TMDB" },
  { id: "tvdb", label: "TVDB" },
  { id: "bangumi", label: "Bangumi" },
  { id: "anilist", label: "AniList" },
];
type SourceState = { items: MediaSearchItem[] | null; error: string | null };
const emptySourceStates = (): Record<MediaSource, SourceState> =>
  Object.fromEntries(MEDIA_SOURCES.map(({ id }) => [id, { items: null, error: null }])) as Record<MediaSource, SourceState>;

/**
 * 搜索结果页「影视」垂直：豆瓣 + TMDB 双来源搜索，上下两个分区展示。
 *
 * 与站点资源垂直（SearchResults）并列挂在 /search 页的选项卡下。后端一次
 * 并行搜索两个来源并返回各自状态：豆瓣中文条目全、TMDB 对外语片/动画覆盖更好且
 * 自带年份和类型。两边没有可靠的对齐键（豆瓣轻量结果无 IMDB ID），不做合并
 * 去重——分区并列反而让用户一眼对比。单边失败/为空只在该分区内提示（TMDB
 * 未配置 Key 时的引导也走这里），不拖垮另一边。
 *
 * 两种数据来源：
 *   - 实时搜索（snapshotId 为空）：统一搜索两个来源并只保存一条历史；
 *   - 快照回放（snapshotId 非空，点历史进入）：读历史留存的结果快照，
 *     不访问上游，头部出「X 前的快照 · 重新搜索」提示。
 *
 * 空态是媒体优先设计的关键出口：用户搜软件名等非影视关键词时两边都会空手，
 * 必须给一个显眼的「去站点资源搜索」入口，否则用户会以为搜索坏了。
 */
export function MediaSearchResults({
  keyword,
  snapshotId,
  onResearch,
  onSwitchToTorrent,
}: {
  keyword: string;
  /** 非空 = 回放该条历史的媒体结果快照，而非发起实时搜索 */
  snapshotId?: string;
  /** 快照提示条的「重新搜索」：切回实时搜索（丢掉 snapshot 参数）；不传则不渲染该按钮 */
  onResearch?: () => void;
  /** 切到「站点资源」垂直（空态/出错时的逃生入口） */
  onSwitchToTorrent?: () => void;
}) {
  const scrollRef = useScrollRestoration(`search:media:${keyword}:${snapshotId ?? "live"}`);
  const [params, setParams] = useSearchParams();
  const sortOrder = (params.get("sort") as "year_desc" | "year_asc" | null) ?? "default";
  // 每个来源各自三态：null = 加载中；[] = 无结果；error 非空 = 该分区失败
  const [sources, setSources] = useState<Record<MediaSource, SourceState>>(emptySourceStates);
  // 快照回放态：非空 = 当前展示的是历史快照（值为快照生成时间，供提示条换算年龄）
  const [snapshotAt, setSnapshotAt] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    setSources(emptySourceStates());
    setSnapshotAt(null);

    // 实时搜索：一个语义化请求返回双来源结果与独立状态，单边失败不丢另一边。
    const searchLive = () => {
      return searchTitles(
        keyword,
        { provider: "all", saveHistory: true },
        { signal: controller.signal },
      ).then(({ items, providers }) => {
        if (controller.signal.aborted) return;
        setSources(Object.fromEntries(MEDIA_SOURCES.map(({ id }) => {
          const status = providers.find((provider) => provider.provider === id);
          return [id, {
            items: items.filter((item) => item.source === id),
            error: status && !status.success ? status.message ?? `${id} 搜索失败` : null,
          }];
        })) as Record<MediaSource, SourceState>);
      });
    };

    // 快照回放：结果 DTO 在 API 层统一转换成前端 MediaSearchItem。
    const load =
      snapshotId != null
        ? getTitleSearchHistoryResults(snapshotId)
            .then((snap) => {
              if (controller.signal.aborted) return;
              const items = snap.items;
              setSnapshotAt(snap.snapshot_at);
              setSources(Object.fromEntries(MEDIA_SOURCES.map(({ id }) => [
                id,
                { items: items.filter((item) => item.source === id), error: null },
              ])) as Record<MediaSource, SourceState>);
            })
        : searchLive();

    load.catch((reason: Error) => {
      if (controller.signal.aborted) return;
      const message = reason.message || "影视搜索失败，请稍后重试";
      setSources(Object.fromEntries(MEDIA_SOURCES.map(({ id }) => [id, { items: null, error: message }])) as Record<MediaSource, SourceState>);
    });
    return () => controller.abort();
  }, [keyword, snapshotId]);

  const allSettled = MEDIA_SOURCES.every(({ id }) => sources[id].items !== null || sources[id].error !== null);
  const hasAnyResult = MEDIA_SOURCES.some(({ id }) => (sources[id].items?.length ?? 0) > 0);
  const allEmpty = allSettled && !hasAnyResult;
  const anyError = MEDIA_SOURCES.map(({ id }) => sources[id].error).find(Boolean) ?? null;

  const sortItems = (items: MediaSearchItem[] | null) => {
    if (!items) return null;
    if (sortOrder === "default") return items;
    return [...items].sort((a, b) => {
      const ya = a.year ?? -1;
      const yb = b.year ?? -1;
      if (sortOrder === "year_desc") {
        return yb - ya;
      }
      return ya - yb;
    });
  };

  return (
    <div className="relative flex h-full flex-col">
      {/* 状态行：与站点资源垂直的头部同构（关键词 + 快照提示） */}
      <header className="shrink-0 px-6 pb-3 pt-4 max-md:px-4 max-md:pt-3">
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
          <h1 className="text-on-image text-title-lg font-semibold tracking-[-0.01em] text-white">
            “{keyword}”
          </h1>

          {/* 快照提示：药丸 + 重新搜索（与站点资源垂直同款视觉） */}
          <div className="ml-auto flex flex-wrap items-center gap-3">
            {hasAnyResult && (
              <div className="flex items-center gap-1.5 text-caption text-white/60">
                <span>排序:</span>
                <select
                  value={sortOrder}
                  onChange={(e) => {
                    const next = new URLSearchParams(params);
                    if (e.target.value === "default") {
                      next.delete("sort");
                    } else {
                      next.set("sort", e.target.value);
                    }
                    setParams(next, { replace: true });
                  }}
                  className="rounded-lg border border-white/10 bg-white/5 px-2 py-1 text-caption text-white/90 backdrop-blur-md outline-none transition focus:border-[var(--accent)]"
                >
                  <option value="default" className="bg-[#12141a] text-white">默认相关度</option>
                  <option value="year_desc" className="bg-[#12141a] text-white">年份（最新优先）</option>
                  <option value="year_asc" className="bg-[#12141a] text-white">年份（最早优先）</option>
                </select>
              </div>
            )}

            {snapshotAt && (
              <div className="flex items-center gap-2">
                <span
                  title="这是历史留存的结果快照，来源站数据（评分/海报）可能已变化"
                  className="flex items-center gap-1.5 rounded-full border border-[#6aa7ff]/30 bg-[#6aa7ff]/12 px-2.5 py-1 text-caption text-[#b9d4ff] backdrop-blur-sm"
                >
                  <svg
                    viewBox="0 0 24 24"
                    className="size-[13px] shrink-0"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth={1.8}
                    strokeLinecap="round"
                    aria-hidden="true"
                  >
                    <circle cx="12" cy="12" r="8.5" />
                    <path d="M12 7.5V12l3 2" />
                  </svg>
                  {formatRelativeTime(snapshotAt)}的快照
                </span>
                {onResearch && (
                  <button
                    type="button"
                    onClick={onResearch}
                    className="btn-accent rounded-full px-2.5 py-1 text-caption font-medium"
                  >
                    重新搜索
                  </button>
                )}
              </div>
            )}
          </div>
        </div>
      </header>

      <div
        ref={scrollRef}
        className="scroll-thin scroll-safe relative min-h-0 flex-1 overflow-y-auto px-6 pb-6 max-md:px-4"
      >
        {allEmpty ? (
          <MediaSearchEmpty
            title={anyError ? "影视搜索出错" : "没有找到相关影视条目"}
            hint={anyError ?? "换个关键词试试；如果找的是非影视资源，可以直接搜索站点。"}
            onSwitchToTorrent={onSwitchToTorrent}
          />
        ) : (
          <div className="flex flex-col gap-7">
            {MEDIA_SOURCES.map(({ id, label }) => (
              <MediaSourceSection
                key={id}
                label={label}
                items={sortItems(sources[id].items)}
                error={sources[id].error}
                hrefOf={(item) => id === "douban"
                  ? `/media/douban/${item.id}`
                  : id === "tmdb"
                    ? `/media/${item.type ?? "movie"}/${item.id}`
                    : `/media/${item.type ?? "movie"}/${item.id}?source=${id}`}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** 单个来源分区：来源徽标 + 计数子标题，正文按「加载 / 出错 / 空 / 网格」四态渲染。 */
function MediaSourceSection({
  label,
  items,
  error,
  hrefOf,
}: {
  label: string;
  /** null = 加载中 */
  items: MediaSearchItem[] | null;
  error: string | null;
  hrefOf: (item: MediaSearchItem) => string;
}) {
  return (
    <section>
      <div className="mb-3 flex items-center gap-2.5">
        <span className="rounded-full bg-black/30 px-2.5 py-0.5 text-caption text-[var(--accent)] backdrop-blur-sm">
          {label}
        </span>
        {items && items.length > 0 && (
          <span className="text-on-image text-sub text-[rgba(243,245,249,0.75)]">
            共 {items.length} 条结果
          </span>
        )}
      </div>

      {!items && !error && <MediaSearchSkeleton />}
      {error && (
        <p className="text-on-image text-sub leading-relaxed text-[rgba(243,245,249,0.6)]">
          {error}
        </p>
      )}
      {items?.length === 0 && (
        <p className="text-on-image text-sub text-[rgba(243,245,249,0.6)]">
          该来源没有找到相关条目
        </p>
      )}
      {items && items.length > 0 && (
        <div className="grid gap-x-4 gap-y-7 pt-1 [grid-template-columns:repeat(auto-fill,minmax(148px,1fr))]">
          {items.map((item) => (
            <div key={item.id} className="min-w-0">
              <PosterCardVisual item={item} href={hrefOf(item)} />
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

/** 空态/错误态：说明文案 + 「搜索站点资源」逃生按钮。 */
function MediaSearchEmpty({
  title,
  hint,
  onSwitchToTorrent,
}: {
  title: string;
  hint: string;
  onSwitchToTorrent?: () => void;
}) {
  return (
    <div className="flex flex-col items-center pt-24 text-center">
      <p className="text-on-image text-body-lg font-semibold text-white">{title}</p>
      <p className="text-on-image mt-1.5 text-sub text-[rgba(243,245,249,0.7)]">{hint}</p>
      {onSwitchToTorrent && (
        <button
          type="button"
          onClick={onSwitchToTorrent}
          className="btn-accent mt-5 rounded-full px-4 py-1.5 text-sub font-semibold"
        >
          搜索站点资源
        </button>
      )}
    </div>
  );
}

function MediaSearchSkeleton() {
  return (
    <div className="grid gap-x-4 gap-y-7 pt-1 [grid-template-columns:repeat(auto-fill,minmax(148px,1fr))]">
      {Array.from({ length: 7 }, (_, index) => (
        <div key={index} className="aspect-[2/3] animate-pulse rounded-2xl bg-white/[0.05]" />
      ))}
    </div>
  );
}
