import { useEffect, useMemo, useState } from "react";

import { Modal } from "@/components/modal";
import {
  previewLibraryOrganize,
  type MediaLibrary,
  type OrganizePreviewResult,
} from "@/lib/api/libraries";
import {
  buildOrganizeRenames,
  organizeTargets,
  type OrganizeRenameRequest,
} from "@/lib/library-organize";

/**
 * 整理确认：选择目标媒体库与目标根目录（默认当前库与初始预览根；切换目标后按后端重新预览）。
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
  const initialRoot = preview.root ?? library.root_paths[0] ?? "";
  const [targetId, setTargetId] = useState(library.id);
  const [targetRoot, setTargetRoot] = useState(initialRoot);
  const [activePreview, setActivePreview] = useState<OrganizePreviewResult>(preview);
  const [loading, setLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);

  useEffect(() => {
    setTargetId(library.id);
    setTargetRoot(initialRoot);
    setActivePreview(preview);
    setLoading(false);
    setPreviewError(null);
  }, [library.id, initialRoot, preview]);

  const target = targets.find((t) => t.id === targetId) ?? library;
  const effectiveRoot = target.root_paths.includes(targetRoot)
    ? targetRoot
    : (target.root_paths[0] ?? initialRoot);
  const isInitialSelection = target.id === library.id && effectiveRoot === initialRoot;

  useEffect(() => {
    if (isInitialSelection) {
      setActivePreview(preview);
      setLoading(false);
      setPreviewError(null);
      return;
    }
    const controller = new AbortController();
    setLoading(true);
    setPreviewError(null);
    void previewLibraryOrganize(library.id, {
      targetLibraryId: target.id,
      targetRoot: effectiveRoot || undefined,
      signal: controller.signal,
    })
      .then((nextPreview) => {
        if (controller.signal.aborted) return;
        setActivePreview(nextPreview);
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setPreviewError(error instanceof Error ? error.message : "预览整理失败");
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [library.id, target.id, effectiveRoot, isInitialSelection, preview]);

  const renames = useMemo(
    () => buildOrganizeRenames(activePreview.renames, library, target),
    [activePreview.renames, library, target],
  );
  const crossLibrary = target.id !== library.id;
  const destRoot = activePreview.root ?? effectiveRoot;

  return (
    <Modal open onClose={onClose} label={`整理「${library.name}」`}>
      <div className="p-5">
        <h2 className="text-title font-bold text-white">
          {loading
            ? `正在计算「${library.name}」的整理计划…`
            : `整理「${library.name}」的 ${activePreview.renames.length} 个文件`}
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
          value={target.id}
          onChange={(event) => {
            const nextId = event.target.value;
            const nextTarget = targets.find((t) => t.id === nextId) ?? library;
            setTargetId(nextId);
            setTargetRoot(nextTarget.id === library.id ? initialRoot : (nextTarget.root_paths[0] ?? ""));
          }}
          className="mt-1 w-full rounded-lg border border-white/[0.09] bg-transparent px-3 py-2 text-ui"
        >
          {targets.map((t) => (
            <option key={t.id} value={t.id}>
              {t.id === library.id ? `${t.name}（当前库）` : t.name}
            </option>
          ))}
        </select>
        {target.root_paths.length > 1 && (
          <>
            <label
              className="mt-3 block text-sub text-[var(--text-muted)]"
              htmlFor="organize-target-root"
            >
              目标根目录
            </label>
            <select
              id="organize-target-root"
              aria-label="目标根目录"
              value={effectiveRoot}
              onChange={(event) => setTargetRoot(event.target.value)}
              className="mt-1 w-full rounded-lg border border-white/[0.09] bg-transparent px-3 py-2 text-ui"
            >
              {target.root_paths.map((rootPath) => (
                <option key={rootPath} value={rootPath}>
                  {rootPath}
                </option>
              ))}
            </select>
          </>
        )}
        {loading ? (
          <p className="mt-2 text-caption text-[var(--text-muted)]">
            正在按所选目标重新计算整理计划…
          </p>
        ) : previewError ? (
          <p className="mt-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3 py-2 text-sub text-red-200">
            {previewError}
          </p>
        ) : activePreview.renames.length === 0 ? (
          <p className="mt-2 text-caption text-[var(--text-faint)]">
            {isInitialSelection
              ? "当前库下文件已符合命名模板；如需迁移，请选择其他目标媒体库或根目录。"
              : "目标位置下的文件已符合模板，无需移动。"}
          </p>
        ) : crossLibrary ? (
          <p className="mt-2 text-caption text-[var(--text-faint)]">
            文件将移动到「{target.name}」：{destRoot}
          </p>
        ) : effectiveRoot && effectiveRoot !== initialRoot ? (
          <p className="mt-2 text-caption text-[var(--text-faint)]">
            文件将移动到根目录：{destRoot}
          </p>
        ) : null}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" onClick={onClose} className="btn-glass px-4 py-2 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={loading || Boolean(previewError) || renames.length === 0}
            onClick={() => onSubmit(renames)}
            className="btn-accent rounded-full px-4 py-2 text-ui font-semibold disabled:opacity-40"
          >
            {loading ? "预览中…" : "开始整理"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
