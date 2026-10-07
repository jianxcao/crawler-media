"use client";

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { useConfirm, usePrompt, useToast } from "@/components/feedback";
import {
  createCollection,
  deleteCollection,
  listCollections,
  type CollectionSummary,
} from "@/lib/api/collections";
import { imageUrl } from "@/lib/image-proxy";

export function CollectionsPage() {
  const toast = useToast();
  const confirm = useConfirm();
  const prompt = usePrompt();
  const [collections, setCollections] = useState<CollectionSummary[] | null>(null);

  const reload = useCallback(() => {
    listCollections()
      .then(setCollections)
      .catch((error) => {
        toast.error(error instanceof Error ? error.message : "加载合集失败");
        setCollections([]);
      });
  }, [toast]);

  useEffect(() => {
    reload();
  }, [reload]);

  const create = async () => {
    const name = await prompt({ title: "新建合集", placeholder: "合集名，如「周末片单」" });
    if (!name) return;
    try {
      await createCollection(name);
      toast.success("合集已创建");
      reload();
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "创建失败");
    }
  };

  const remove = async (collection: CollectionSummary) => {
    const ok = await confirm({
      title: `删除合集「${collection.name}」？`,
      description: "合集里的作品不会被删除，只是从合集移除。",
      confirmLabel: "删除",
      tone: "danger",
    });
    if (!ok) return;
    try {
      await deleteCollection(collection.id);
      toast.success("合集已删除");
      reload();
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "删除失败");
    }
  };

  if (collections === null) {
    return (
      <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
        <BrandLoader className="size-5" />
        正在加载合集…
      </div>
    );
  }

  return (
    <div className="scroll-thin flex-1 overflow-y-auto px-6 pb-10 pt-7 max-md:px-4 max-md:pt-4">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            合集
          </h2>
          <p className="mt-1.5 text-ui text-[var(--text-muted)]">
            把自己收藏的作品按主题归组，例如「周末片单」「给朋友推荐」。
          </p>
        </div>
        <button
          type="button"
          onClick={() => void create()}
          className="btn-accent inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full px-4 text-sub font-semibold"
        >
          新建合集
        </button>
      </div>

      {collections.length === 0 ? (
        <div className="mt-16 flex flex-col items-center gap-3 text-center text-ui text-[var(--text-muted)]">
          <p>还没有合集</p>
          <button type="button" onClick={() => void create()} className="text-[var(--accent)]">
            创建一个
          </button>
        </div>
      ) : (
        <div className="mt-6 grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
          {collections.map((collection) => (
            <div key={collection.id} className="group relative">
              <Link to={`/collections/${collection.id}`} className="block">
                <div className="aspect-[2/3] overflow-hidden rounded-2xl border border-white/[0.07] bg-white/[0.04]">
                  {collection.cover_url ? (
                    <img
                      src={imageUrl(collection.cover_url)}
                      alt=""
                      className="h-full w-full object-cover"
                    />
                  ) : (
                    <div className="grid h-full w-full place-items-center text-[40px] text-white/15">
                      {collection.name.slice(0, 1)}
                    </div>
                  )}
                </div>
                <p className="mt-2 truncate text-sub font-medium text-white/90">{collection.name}</p>
                <p className="text-caption text-white/40">
                  {collection.item_count} 部作品
                </p>
              </Link>
              <button
                type="button"
                onClick={() => void remove(collection)}
                aria-label={`删除 ${collection.name}`}
                className="absolute right-2 top-2 hidden rounded-full bg-black/50 px-2 py-0.5 text-caption text-white/70 backdrop-blur transition group-hover:block hover:text-white"
              >
                删除
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
