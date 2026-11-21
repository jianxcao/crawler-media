"use client";

import { useCallback, useEffect, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { useConfirm, useToast } from "@/components/feedback";
import {
  deleteDuplicateFile,
  type DuplicateEntryGroup,
  listLibraries,
  listLibraryDuplicates,
  type MediaLibrary,
} from "@/lib/api/libraries";
import { formatManageBytes } from "@/lib/library-manage-helpers";

/**
 * 重复文件：同一 (media, season, episode) 单元多份文件，选择保留者，
 * 其余直接物理删除。
 */
export function DuplicatesPanel() {
  const toast = useToast();
  const confirm = useConfirm();
  const [libraries, setLibraries] = useState<MediaLibrary[]>([]);
  const [libraryId, setLibraryId] = useState("");
  const [groups, setGroups] = useState<DuplicateEntryGroup[] | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void listLibraries(undefined, { manage: true })
      .then((libs) => {
        setLibraries(libs);
        if (libs.length > 0) setLibraryId(libs[0].id);
      })
      .catch(() => undefined);
  }, []);

  const reload = useCallback(() => {
    if (!libraryId) return;
    void listLibraryDuplicates(libraryId)
      .then(setGroups)
      .catch((error) => toast.error(`读取重复文件失败：${(error as Error).message}`));
  }, [libraryId, toast]);

  useEffect(() => {
    reload();
  }, [reload]);

  const remove = async (fileId: string, title: string) => {
    if (busy) return;
    const ok = await confirm({
      title: "删除这份重复文件？",
      description: "该文件将直接从磁盘物理删除，无法通过回收站恢复；做种原盘文件不受影响。",
      confirmLabel: "永久删除",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await deleteDuplicateFile(libraryId, fileId);
      toast.success(`「${title}」已删除`);
      reload();
    } catch (error) {
      toast.error((error as Error).message || "操作失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="px-6 pt-4 max-md:px-4">
      <label className="mb-2 block text-sub font-medium text-[var(--text-muted)]">选择媒体库</label>
      <select
        value={libraryId}
        onChange={(e) => setLibraryId(e.target.value)}
        className="w-full max-w-sm rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
      >
        {libraries.map((lib) => (
          <option key={lib.id} value={lib.id}>
            {lib.name}
          </option>
        ))}
      </select>

      {groups === null ? (
        <div className="flex items-center justify-center gap-2.5 py-14 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在扫描重复文件…
        </div>
      ) : groups.length === 0 ? (
        <p className="py-10 text-center text-ui text-[var(--text-muted)]">这个库里没有重复文件</p>
      ) : (
        <div className="mt-3 space-y-3">
          {groups.map((group) => (
            <div key={group.media_item_id} className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4">
              <p className="text-ui font-semibold text-white/90">
                {group.title}
                {group.season != null && (
                  <span className="tnum ml-2 text-caption text-white/50">
                    S{String(group.season).padStart(2, "0")}E{String(group.episode).padStart(2, "0")}
                  </span>
                )}
              </p>
              <div className="mt-2 space-y-1.5">
                {group.files.map((file) => (
                  <div key={file.file_id} className="flex items-center gap-3">
                    <div className="min-w-0 flex-1">
                      <p className="truncate font-mono text-caption text-white/60" title={file.path}>
                        {file.path}
                      </p>
                      <p className="text-caption text-white/35">
                        {[file.resolution, file.codec?.toUpperCase(), file.hdr]
                          .filter(Boolean)
                          .join(" · ") || "未知规格"}
                        {" · "}
                        {formatManageBytes(file.size_bytes)}
                      </p>
                    </div>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void remove(file.file_id, group.title)}
                      className="shrink-0 rounded-lg border border-red-400/25 px-3 py-1.5 text-caption font-medium text-red-300 transition hover:bg-red-500/10 disabled:opacity-40"
                    >
                      删除此版本
                    </button>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
