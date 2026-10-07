"use client";

import { useEffect, useState } from "react";

import type { LibraryHit } from "@/lib/api/search";
import { searchLibraryItems } from "@/lib/api/search";
import { LIBRARY_KIND_LABELS } from "@/lib/media-types";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

/**
 * 搜索结果页「媒体库」垂直：跨全部媒体库搜索已入库条目。
 *
 * 与影视（MediaSearchResults）、站点资源（SearchResults）并列挂在 /search
 * 页的选项卡下，回答的问题是「这部片我有没有」。数据全在本地（标题/原名
 * 子串匹配），毫秒级返回，没有快照与历史——搜自己的库是翻家底，不值得回放。
 *
 * 新契约 searchLibraryItems 返回扁平 LibraryHit（不再按库分组的
 * LibrarySearchGroup）：每行自带 media_item_id/title/kind/path/season/episode，
 * 没有库归属与海报信息，因此改为扁平行列表展示。
 *
 * 空态的出口指向「影视」垂直：库里没有 ≈ 想要但还没入手，下一步自然是
 * 去影视条目搜索并订阅/下载。
 */
export function LibrarySearchResults({
  keyword,
  onSwitchToMedia,
}: {
  keyword: string;
  /** 切到「影视」垂直（空态时的出口：库里没有 → 去找来） */
  onSwitchToMedia?: () => void;
}) {
  const scrollRef = useScrollRestoration(`search:library:${keyword}`);
  // null = 加载中；[] = 无结果；error 非空 = 请求失败
  const [items, setItems] = useState<LibraryHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setItems(null);
    setError(null);
    searchLibraryItems(keyword)
      .then((list) => {
        if (!cancelled) setItems(list);
      })
      .catch((reason: Error) => {
        if (!cancelled) setError(reason.message || "媒体库搜索失败，请稍后重试");
      });
    return () => {
      cancelled = true;
    };
  }, [keyword]);

  const empty = items !== null && items.length === 0;

  return (
    <div className="relative flex h-full flex-col">
      {/* 状态行：与另外两个垂直的头部同构（关键词） */}
      <header className="shrink-0 px-6 pb-3 pt-4 max-md:px-4 max-md:pt-3">
        <h1 className="text-on-image text-title-lg font-semibold tracking-[-0.01em] text-white">
          “{keyword}”
        </h1>
      </header>

      <div
        ref={scrollRef}
        className="scroll-thin scroll-safe relative min-h-0 flex-1 overflow-y-auto px-6 pb-6 max-md:px-4"
      >
        {items === null && !error && <LibrarySearchSkeleton />}
        {(error || empty) && (
          <div className="flex flex-col items-center pt-24 text-center">
            <p className="text-on-image text-body-lg font-semibold text-white">
              {error ? "媒体库搜索出错" : "媒体库中没有找到相关影片"}
            </p>
            <p className="text-on-image mt-1.5 text-sub text-[rgba(243,245,249,0.7)]">
              {error ?? "已入库条目按标题和原名匹配；库里还没有的片子，去影视条目里找。"}
            </p>
            {onSwitchToMedia && (
              <button
                type="button"
                onClick={onSwitchToMedia}
                className="btn-accent mt-5 rounded-full px-4 py-1.5 text-sub font-semibold"
              >
                搜索影视条目
              </button>
            )}
          </div>
        )}
        {items !== null && items.length > 0 && (
          <ul className="space-y-2">
            {items.map((hit) => (
              <LibraryHitRow key={hit.media_item_id} hit={hit} />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

/** 一行命中：类型徽标 + 标题 + 季集定位 + 库内路径。 */
function LibraryHitRow({ hit }: { hit: LibraryHit }) {
  const parts: string[] = [];
  if (hit.kind === "tv" && hit.season != null) {
    parts.push(
      hit.episode != null ? `第 ${hit.season} 季 · 第 ${hit.episode} 集` : `第 ${hit.season} 季`,
    );
  }
  if (hit.path) parts.push(hit.path);
  return (
    <li className="flex items-center gap-3 rounded-2xl border border-white/[0.06] bg-[rgba(16,18,25,0.82)] px-4 py-3">
      <span className="shrink-0 rounded-md bg-white/[0.06] px-1.5 py-0.5 text-micro font-medium text-[var(--accent-2)]">
        {LIBRARY_KIND_LABELS[hit.kind] ?? hit.kind}
      </span>
      <span className="min-w-0 flex-1 truncate text-ui font-medium leading-5 text-[var(--text)]">
        {hit.title}
      </span>
      {parts.length > 0 && (
        <span
          title={parts.join(" · ")}
          className="tnum hidden shrink-0 truncate text-caption text-[var(--text-faint)] sm:block"
        >
          {parts.join(" · ")}
        </span>
      )}
    </li>
  );
}

function LibrarySearchSkeleton() {
  return (
    <div className="space-y-2" aria-hidden="true">
      {Array.from({ length: 7 }, (_, index) => (
        <div
          key={index}
          className="h-[52px] animate-pulse rounded-2xl bg-white/[0.05]"
          style={{ opacity: 1 - index * 0.1 }}
        />
      ))}
    </div>
  );
}
