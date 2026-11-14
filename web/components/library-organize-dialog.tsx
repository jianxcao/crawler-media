import { useEffect, useMemo, useState } from "react";

import { Modal } from "@/components/modal";
import type { MediaLibrary, OrganizePreviewResult } from "@/lib/api/libraries";
import { buildOrganizeRenames, organizeTargets, type OrganizeRenameRequest } from "@/lib/library-organize";

/**
 * 整理确认：选择目标媒体库（默认当前库；同类型的其他库走后端跨库持久改挂）。
 */
export function LibraryOrganizeDialog({
  library,
  libraries,
  preview,
  onClose,
  onSubmit,
}: {
  library: MediaLibrary;
  libraries: readonly MediaLibrary[];
  preview: OrganizePreviewResult;
  onClose: () => void;
  onSubmit: (renames: OrganizeRenameRequest[]) => void;
}) {
  const targets = useMemo(() => organizeTargets(library, libraries), [library, libraries]);
  const [targetId, setTargetId] = useState(library.id);
  useEffect(() => setTargetId(library.id), [library.id]);
  const target = targets.find((t) => t.id === targetId) ?? library;
  const renames = useMemo(
    () => buildOrganizeRenames(preview.renames, library, target),
    [preview.renames, library, target],
  );
  const crossLibrary = target.id !== library.id;
  return (
    <Modal open onClose={onClose} label={`整理「${library.name}」`}>
      <div className="p-5">
        <h2 className="text-title font-bold text-white">
          整理「{library.name}」的 {preview.renames.length} 个文件
        </h2>
        <p className="mt-1 text-sub text-[var(--text-muted)]">
          按命名模板重命名；原有路径会同步到台账与订阅记录。
        </p>
        <label className="mt-4 block text-sub text-[var(--text-muted)]" htmlFor="organize-target">
          目标媒体库
        </label>
        <select
          id="organize-target"
          aria-label="目标媒体库"
          value={targetId}
          onChange={(event) => setTargetId(event.target.value)}
          className="mt-1 w-full rounded-lg border border-white/[0.09] bg-transparent px-3 py-2 text-ui"
        >
          {targets.map((t) => (
            <option key={t.id} value={t.id}>
              {t.id === library.id ? `${t.name}（当前库）` : t.name}
            </option>
          ))}
        </select>
        {crossLibrary && (
          <p className="mt-2 text-caption text-[var(--text-faint)]">
            文件将移动到「{target.name}」：{target.root_paths[0]}
            {renames.length < preview.renames.length &&
              `；${preview.renames.length - renames.length} 个不在源库根目录下，将跳过`}
          </p>
        )}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" onClick={onClose} className="btn-glass px-4 py-2 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={renames.length === 0}
            onClick={() => onSubmit(renames)}
            className="btn-accent rounded-full px-4 py-2 text-ui font-semibold disabled:opacity-40"
          >
            开始整理
          </button>
        </div>
      </div>
    </Modal>
  );
}
