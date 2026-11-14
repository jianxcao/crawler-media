"use client";

import { ArtworkImage } from "@/components/artwork-image";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import {
  listCollectionItems,
  removeCollectionItem,
  type CollectionItem,
} from "@/lib/api/collections";
import { CollectionPager } from "@/lib/collection-pager";
import { imageUrl } from "@/lib/image-proxy";

export function CollectionDetailPage({ idPromise }: { idPromise: Promise<{ id: string }> }) {
  const navigate = useNavigate();
  const toast = useToast();
  const [id, setId] = useState<string | null>(null);
  const pager = useMemo(() => new CollectionPager(listCollectionItems), []);
  const [page, setPage] = useState(pager.state);
  const items = page.items;
  const [removing, setRemoving] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    pager.clear();
    const unsubscribe = pager.subscribe(setPage);
    setId(null);
    setRemoving(null);
    void idPromise.then((params) => {
      if (!alive) return;
      setId(params.id);
      void pager.reset(params.id);
    });
    return () => {
      alive = false;
      unsubscribe();
      pager.clear();
    };
  }, [idPromise, pager]);

  const removeItem = async (item: CollectionItem) => {
    if (!id) return;
    setRemoving(item.media_item_id);
    try {
      await removeCollectionItem(id, item.media_item_id);
      await pager.refresh(id);
      toast.success(`已从合集移除「${item.title}」`);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "移除失败");
    } finally {
      setRemoving(null);
    }
  };

  const hrefOf = (item: CollectionItem) => {
    if (item.library_id) return `/library/${item.library_id}/item/${item.media_item_id}`;
    if (item.kind === "tv") return `/media/tv/${item.media_item_id}`;
    return `/media/movie/${item.media_item_id}`;
  };

  return (
    <div className="scroll-thin flex-1 overflow-y-auto px-6 pb-10 pt-7 max-md:px-4 max-md:pt-4">
      <button
        type="button"
        onClick={() => navigate("/collections")}
        className="mb-4 text-caption text-white/50 hover:text-white"
      >
        ← 返回合集
      </button>
      {items === null && page.error ? (
        <div className="mt-8 text-ui text-[var(--text-muted)]" role="alert">
          <p>{page.error}</p>
          <button type="button" onClick={() => void pager.refresh()} className="mt-3 text-white">重试</button>
        </div>
      ) : items === null ? (
        <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在加载…
        </div>
      ) : (
        <>
          <h2 className="text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            合集 · {page.total} 部
          </h2>
          {items.length === 0 ? (
            <p className="mt-8 text-ui text-[var(--text-muted)]">
              空合集——在条目详情页点「加入合集」把作品放进来。
            </p>
          ) : (
            <div className="mt-6 grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
              {items.map((item) => (
                <div key={item.media_item_id} className="group relative">
                  <Link to={hrefOf(item)} className="block">
                    <div className="aspect-[2/3] overflow-hidden rounded-2xl border border-white/[0.07] bg-white/[0.04]">
                      {item.poster_url ? (
                        <ArtworkImage
                          src={imageUrl(item.poster_url)}
                          alt=""
                          className="h-full w-full object-cover"
                        />
                      ) : (
                        <div className="grid h-full w-full place-items-center px-3 text-center text-sub text-white/25">
                          {item.title}
                        </div>
                      )}
                    </div>
                    <p className="mt-2 truncate text-sub font-medium text-white/90">{item.title}</p>
                    {item.year != null && <p className="text-caption text-white/40">{item.year}</p>}
                  </Link>
                  <button
                    type="button"
                    disabled={removing === item.media_item_id}
                    onClick={() => void removeItem(item)}
                    className="absolute right-2 top-2 hidden rounded-full bg-black/50 px-2 py-0.5 text-caption text-white/70 backdrop-blur transition group-hover:block hover:text-white disabled:opacity-40"
                  >
                    移除
                  </button>
                </div>
              ))}
            </div>
          )}
          {page.error && <p role="alert" className="mt-4 text-ui text-red-300">{page.error}</p>}
          {page.nextCursor && (
            <button
              type="button"
              disabled={page.loading}
              onClick={() => void pager.loadMore()}
              className="mt-6 rounded-xl border border-white/10 px-5 py-2 text-ui text-white/80 disabled:opacity-40"
            >
              {page.loading ? "正在加载…" : page.error ? "重试加载更多" : "加载更多"}
            </button>
          )}
        </>
      )}
    </div>
  );
}
