import type { useConfirm, useToast } from "@/components/feedback";
import {
  applyPathReconciliations,
  applyRootConsolidations,
  type MediaLibrary,
  type OrganizePreviewResult,
  previewLibraryOrganize,
  previewPathReconciliation,
  previewRootConsolidation,
  startLibraryOrganize,
} from "@/lib/api/libraries";
import {
  canOpenOrganizeDialog,
  type OrganizeRenameRequest,
} from "@/lib/library-organize";

/** 整理文件名第一步：预览按命名模板算出的目标名；无可整理文件且无其他目标可选时直接提示。 */
export async function previewOrganize(
  library: MediaLibrary,
  libraries: readonly MediaLibrary[],
  toast: ReturnType<typeof useToast>,
): Promise<OrganizePreviewResult | null> {
  try {
    const preview = await previewLibraryOrganize(library.id);
    if (!canOpenOrganizeDialog(library, libraries, preview)) {
      toast.success(
        preview.already_ok > 0
          ? `「${library.name}」文件名已符合模板`
          : `「${library.name}」没有可整理的文件`,
      );
      return null;
    }
    return preview;
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "整理失败");
    return null;
  }
}

/** 整理文件名第二步：按所选目标库（可跨库）应用重命名。 */
export async function applyOrganize(
  library: MediaLibrary,
  renames: OrganizeRenameRequest[],
  toast: ReturnType<typeof useToast>,
) {
  try {
    const result = await startLibraryOrganize(library.id, renames);
    toast.success(
      `已整理 ${result.applied} 个文件${result.errors.length > 0 ? `，${result.errors.length} 个失败` : ""}`,
    );
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "整理失败");
  }
}

/** 路径核对：台账失联文件 → 库内同名候选 → 确认重关联。 */
export async function reconcileLibrary(
  library: MediaLibrary,
  toast: ReturnType<typeof useToast>,
  confirm: ReturnType<typeof useConfirm>,
) {
  try {
    const { missing } = await previewPathReconciliation(library.id);
    if (missing.length === 0) {
      toast.success(`「${library.name}」台账文件全部在位`);
      return;
    }
    const auto = missing
      .map((row) => ({ from: row.path, to: row.candidates[0] ?? null }))
      .filter((x): x is { from: string; to: string } => x.to != null);
    if (auto.length === 0) {
      toast.error(`有 ${missing.length} 个失联文件，但库内没找到同名候选`);
      return;
    }
    const ok = await confirm({
      title: `重新关联 ${auto.length} 个失联文件？`,
      description: `${missing.length} 个文件不在原位；将按同名候选把台账指向新位置。`,
      confirmLabel: "重新关联",
    });
    if (!ok) return;
    const result = await applyPathReconciliations(library.id, auto);
    toast.success(
      `已重新关联 ${result.reconciled} 个文件${result.errors.length ? `，${result.errors.length} 个失败` : ""}`,
    );
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "路径核对失败");
  }
}

/** 根目录归并：多个根指向同一物理目录时删重复根。 */
export async function consolidateLibrary(
  library: MediaLibrary,
  toast: ReturnType<typeof useToast>,
  confirm: ReturnType<typeof useConfirm>,
): Promise<boolean> {
  try {
    const { duplicates } = await previewRootConsolidation(library.id);
    if (duplicates.length === 0) {
      toast.success(`「${library.name}」没有重复的根目录`);
      return false;
    }
    const mergeRoots = duplicates.map((d) => d.merge);
    const ok = await confirm({
      title: `归并 ${mergeRoots.length} 个重复根目录？`,
      description: duplicates.map((d) => `${d.merge} → 保留 ${d.keep}`).join("；"),
      confirmLabel: "归并",
    });
    if (!ok) return false;
    const result = await applyRootConsolidations(library.id, mergeRoots);
    toast.success(`已归并 ${result.consolidated} 个根目录`);
    return true;
  } catch (error) {
    toast.error(error instanceof Error ? error.message : "根目录归并失败");
    return false;
  }
}
