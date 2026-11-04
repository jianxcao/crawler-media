"use client";

import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";

import type { LibraryHit } from "@/lib/api/search";
import { searchLibraryItems } from "@/lib/api/search";
import { LIBRARY_KIND_LABELS } from "@/lib/media-types";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

/**
 * 搜索结果页「媒体库」垂直：跨全部媒体库搜索已入库条目。
 *
 * 与影视（MediaSearchResults）、站点资源（SearchResults）并列挂在 /search
 * 页的选项卡下，回答的问题是「这部片我有没有」。数据全在本地，支持标题
 * 和演职员搜索；没有快照与历史。
 *
 * 每个命中代表一个 Media，结果按游标分页。
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
  const [cursor, setCursor] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const generation = useRef(0);
  const pending = useRef(false);

  useEffect(() => {
    let cancelled = false;
    generation.current += 1;
    pending.current = false;
    setItems(null);
    setError(null);
    setCursor(null);
    setLoadingMore(false);
    searchLibraryItems(keyword)
      .then((page) => {
        if (!cancelled) {
          setItems(page.items);
          setCursor(page.next_cursor);
        }
      })
      .catch((reason: Error) => {
        if (!cancelled) setError(reason.message || "媒体库搜索失败，请稍后重试");
      });
    return () => {
      cancelled = true;
      generation.current += 1;
    };
  }, [keyword]);

  const loadMore = async () => {
    if (!cursor || pending.current) return;
    const current = generation.current;
    pending.current = true;
    setLoadingMore(true);
    setError(null);
    try {
      const page = await searchLibraryItems(keyword, cursor);
      if (current !== generation.current) return;
      setItems((previous) => previous === null ? page.items : [...previous, ...page.items]);
      setCursor(page.next_cursor);
    } catch (reason) {
      if (current === generation.current) setError(reason instanceof Error ? reason.message : "媒体库搜索失败，请稍后重试");
    } finally {
      if (current === generation.current) {
        pending.current = false;
        setLoadingMore(false);
      }
    }
  };

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
        {((error && items === null) || empty) && (
          <div className="flex flex-col items-center pt-24 text-center">
            <p className="text-on-image text-body-lg font-semibold text-white">
              {error ? "媒体库搜索出错" : "媒体库中没有找到相关影片"}
            </p>
            <p className="text-on-image mt-1.5 text-sub text-[rgba(243,245,249,0.7)]">
              {error ?? "已入库条目按标题和演职员匹配；库里还没有的片子，去影视条目里找。"}
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
        {items !== null && cursor && (
          <div className="mt-4 text-center">
            {error && <p className="mb-2 text-sub text-[var(--text-faint)]">{error}</p>}
            <button type="button" className="btn-accent px-4 py-1.5" disabled={loadingMore} onClick={() => void loadMore()}>
              {loadingMore ? "正在加载…" : error ? "重试加载更多" : "加载更多"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** 一行命中：类型、标题和年份。 */
function LibraryHitRow({ hit }: { hit: LibraryHit }) {
  return (
    <li>
      <Link to={`/library/${hit.library_id}/item/${hit.media_item_id}`} className="flex items-center gap-3 rounded-2xl border border-white/[0.06] bg-[rgba(16,18,25,0.82)] px-4 py-3">
        <span className="shrink-0 rounded-md bg-white/[0.06] px-1.5 py-0.5 text-micro font-medium text-[var(--accent-2)]">
          {LIBRARY_KIND_LABELS[hit.kind] ?? hit.kind}
        </span>
        <span className="min-w-0 flex-1 truncate text-ui font-medium leading-5 text-[var(--text)]">
          {hit.title}
        </span>
        {hit.year !== null && (
          <span className="tnum shrink-0 text-caption text-[var(--text-faint)]">{hit.year}</span>
        )}
      </Link>
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
