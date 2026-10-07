"use client";

import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import {
  listCollectionItems,
  removeCollectionItem,
  type CollectionItem,
} from "@/lib/api/collections";
import { imageUrl } from "@/lib/image-proxy";

export function CollectionDetailPage({ idPromise }: { idPromise: Promise<{ id: string }> }) {
  const navigate = useNavigate();
  const toast = useToast();
  const [id, setId] = useState<string | null>(null);
  const [items, setItems] = useState<CollectionItem[] | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void idPromise.then((params) => {
      if (!alive) return;
      setId(params.id);
      listCollectionItems(params.id)
        .then((rows) => alive && setItems(rows))
        .catch((error) => {
          toast.error(error instanceof Error ? error.message : "加载失败");
          if (alive) setItems([]);
        });
    });
    return () => {
      alive = false;
    };
  }, [idPromise, toast]);

  const removeItem = async (item: CollectionItem) => {
    if (!id) return;
    setRemoving(item.media_item_id);
    try {
      await removeCollectionItem(id, item.media_item_id);
      setItems((current) => (current ?? []).filter((row) => row.media_item_id !== item.media_item_id));
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
      {items === null ? (
        <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在加载…
        </div>
      ) : (
        <>
          <h2 className="text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            合集 · {items.length} 部
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
                        <img
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
        </>
      )}
    </div>
  );
}
