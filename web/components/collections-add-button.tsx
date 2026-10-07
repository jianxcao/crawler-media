"use client";

import { useState } from "react";

import { useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import {
  addCollectionItem,
  createCollection,
  listCollections,
  type CollectionSummary,
} from "@/lib/api/collections";

/** 条目详情页的「加入合集」：弹出我的合集，点选即加入；可现场新建。 */
export function CollectionsAddButton({ mediaItemId }: { mediaItemId: string }) {
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [collections, setCollections] = useState<CollectionSummary[] | null>(null);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");

  const openDialog = async () => {
    setOpen(true);
    try {
      setCollections(await listCollections());
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "加载合集失败");
      setCollections([]);
    }
  };

  const addTo = async (collection: CollectionSummary) => {
    try {
      await addCollectionItem(collection.id, mediaItemId);
      toast.success(`已加入「${collection.name}」`);
      setOpen(false);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "添加失败");
    }
  };

  const createAndAdd = async () => {
    const name = newName.trim();
    if (!name) return;
    setCreating(true);
    try {
      const { id } = await createCollection(name);
      await addCollectionItem(id, mediaItemId);
      toast.success(`已创建并加入「${name}」`);
      setOpen(false);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "创建失败");
    } finally {
      setCreating(false);
    }
  };

  return (
    <>
      <button
        type="button"
        onClick={() => void openDialog()}
        className="btn-glass inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.05] px-3.5 text-sub font-medium backdrop-blur-md transition hover:bg-white/[0.09]"
      >
        + 加入合集
      </button>
      <Modal open={open} onClose={() => setOpen(false)} label="加入合集">
        <div className="space-y-1.5">
          {collections === null ? (
            <p className="py-4 text-center text-sub text-white/40">加载中…</p>
          ) : collections.length === 0 ? (
            <p className="py-2 text-sub text-white/40">还没有合集，先建一个：</p>
          ) : (
            collections.map((collection) => (
              <button
                key={collection.id}
                type="button"
                onClick={() => void addTo(collection)}
                className="glass-row flex w-full items-center justify-between rounded-xl px-3 py-2 text-left text-sub text-white/85 hover:!bg-[var(--glass-fill-hover)]"
              >
                <span className="truncate">{collection.name}</span>
                <span className="shrink-0 text-caption text-white/40">{collection.item_count} 部</span>
              </button>
            ))
          )}
          <div className="flex gap-2 pt-2">
            <input
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder="新合集名…"
              className="w-full rounded-lg border border-white/10 bg-white/[0.04] px-2.5 py-1.5 text-sub text-white placeholder:text-white/30 focus:border-white/25 focus:outline-none"
            />
            <button
              type="button"
              disabled={creating || !newName.trim()}
              onClick={() => void createAndAdd()}
              className="btn-accent shrink-0 rounded-full px-3.5 text-sub font-semibold disabled:opacity-40"
            >
              新建并加入
            </button>
          </div>
        </div>
      </Modal>
    </>
  );
}
