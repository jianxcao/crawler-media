"use client";

import { useEffect, useState } from "react";

import { FolderIcon, TrashIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import {
  deleteLibraryItem,
  type ItemDeleteResult,
  type LibraryItemDetail,
  type LibraryItemFile,
} from "@/lib/api/libraries";
import { deleteLedgerRow } from "@/lib/api/transfers";
import { formatBytes } from "@/lib/format";
import { fileNameOf } from "@/lib/library-item-format";

export function DeleteDialog({
  open,
  detail,
  libraryId,
  onClose,
  onDeleted,
}: {
  open: boolean;
  detail: LibraryItemDetail;
  libraryId: string;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ItemDeleteResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setConfirmed(false);
      setResult(null);
      setError(null);
    }
  }, [open]);

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      setResult(await deleteLibraryItem(libraryId, detail.media_item_id));
    } catch (err) {
      setError(err instanceof Error ? err.message : "删除失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal open={open} onClose={busy ? () => {} : onClose} label="删除影片" width="lg">
      <div className="p-6">
        {result ? (
          <>
            <h3 className="text-title-sm font-semibold text-[var(--text)]">
              {result.errors.length > 0 ? "部分删除完成" : "已从磁盘彻底删除"}
            </h3>
            <div className="scroll-thin mt-4 max-h-56 overflow-y-auto rounded-xl border border-white/[0.08] bg-white/[0.03] p-3.5">
              {result.removed_paths.map((p) => (
                <p
                  key={p}
                  className="tnum break-all py-0.5 font-mono text-caption leading-5 text-white/70"
                >
                  {p}
                </p>
              ))}
              {result.removed_paths.length === 0 && (
                <p className="text-sub text-[var(--text-muted)]">没有删除任何磁盘路径</p>
              )}
            </div>
            {result.errors.length > 0 && (
              <div className="mt-3 space-y-1 text-sub leading-5 text-[#ff9f9f]">
                {result.errors.map((e) => (
                  <p key={e}>{e}</p>
                ))}
              </div>
            )}
            <p className="mt-3 text-sub text-[var(--text-muted)]">
              已清理 {result.rows_deleted} 条台账，释放 {formatBytes(result.freed_bytes)}。
            </p>
            <div className="mt-5 flex justify-end">
              <button
                type="button"
                onClick={onDeleted}
                className="btn-accent rounded-full px-5 py-2 text-ui font-semibold"
              >
                回到库存页
              </button>
            </div>
          </>
        ) : (
          <>
            <h3 className="flex items-center gap-2 text-title-sm font-semibold text-[var(--text)]">
              <TrashIcon className="size-4.5 text-[#ff9f9f]" />
              删除「{detail.title}」
            </h3>
            <p className="mt-3 text-ui leading-6 text-white/80">
              这不是「从列表移除」——将把下列目录从磁盘
              <span className="font-semibold text-[#ff9f9f]">彻底删除</span>
              ，包括视频文件、NFO、海报、字幕等全部刮削产物，共{" "}
              <span className="tnum font-semibold">{detail.files.length}</span> 个媒体文件。
              此操作不可恢复。
            </p>
            <div className="scroll-thin mt-4 max-h-40 overflow-y-auto rounded-xl border border-white/[0.08] bg-white/[0.03] p-3.5">
              {detail.files.map((f) => (
                <p
                  key={f.file_id}
                  className="tnum flex items-start gap-1.5 break-all py-0.5 font-mono text-caption leading-5 text-white/70"
                >
                  <FolderIcon className="mt-0.5 size-3.5 shrink-0 text-white/40" />
                  {f.path}
                </p>
              ))}
            </div>
            <label className="mt-4 flex cursor-pointer items-start gap-2.5 text-sub leading-6 text-white/80">
              <input
                type="checkbox"
                checked={confirmed}
                onChange={(e) => setConfirmed(e.target.checked)}
                className="mt-1 size-4 accent-[#ff6b6b]"
              />
              我已明白：以上目录及其中全部文件将被永久删除，无法恢复。
            </label>
            {error && <p className="mt-3 text-sub text-[#ff9f9f]">{error}</p>}
            <div className="mt-5 flex justify-end gap-3">
              <button
                type="button"
                onClick={onClose}
                disabled={busy}
                className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
              >
                取消
              </button>
              <button
                type="button"
                onClick={run}
                disabled={!confirmed || busy}
                className="flex items-center gap-2 rounded-full bg-[var(--danger-solid)] px-5 py-2 text-ui font-semibold text-white transition hover:bg-[var(--danger-solid-hover)] disabled:cursor-not-allowed disabled:opacity-40"
              >
                {busy && (
                  <span className="size-3.5 animate-spin rounded-full border-2 border-white/30 border-t-white" />
                )}
                {busy ? "正在删除…" : "彻底删除"}
              </button>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

export function DeleteFileDialog({
  file,
  onClose,
  onDeleted,
}: {
  file: LibraryItemFile;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setConfirmed(false);
    setError(null);
  }, [file.file_id]);

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      await deleteLedgerRow(file.file_id, true);
      onDeleted();
    } catch (err) {
      setError(err instanceof Error ? err.message : "删除失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal open onClose={busy ? () => {} : onClose} label="删除单个文件" width="lg">
      <div className="p-6">
        <h3 className="flex items-center gap-2 text-title-sm font-semibold text-[var(--text)]">
          <TrashIcon className="size-4.5 text-[#ff9f9f]" />
          删除文件「{fileNameOf(file.path)}」
        </h3>
        <p className="mt-3 text-ui leading-6 text-white/80">
          将从磁盘和媒体库台账中<span className="font-semibold text-[#ff9f9f]">永久删除</span>
          下列单个文件，其他版本文件会保留。此操作不可恢复。
        </p>
        <div className="mt-4 rounded-xl border border-white/[0.08] bg-white/[0.03] p-3.5">
          <p className="tnum break-all font-mono text-caption leading-5 text-white/70">
            {file.path}
          </p>
        </div>
        <label className="mt-4 flex cursor-pointer items-start gap-2.5 text-sub leading-6 text-white/80">
          <input
            type="checkbox"
            checked={confirmed}
            onChange={(e) => setConfirmed(e.target.checked)}
            className="mt-1 size-4 accent-[#ff6b6b]"
          />
          我已明白：该文件将被永久删除，无法恢复。
        </label>
        {error && <p className="mt-3 text-sub text-[#ff9f9f]">{error}</p>}
        <div className="mt-5 flex justify-end gap-3">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            取消
          </button>
          <button
            type="button"
            onClick={() => void run()}
            disabled={!confirmed || busy}
            className="flex items-center gap-2 rounded-full bg-[var(--danger-solid)] px-5 py-2 text-ui font-semibold text-white transition hover:bg-[var(--danger-solid-hover)] disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy && (
              <span className="size-3.5 animate-spin rounded-full border-2 border-white/30 border-t-white" />
            )}
            {busy ? "正在删除…" : "删除此文件"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
