/**
 * 整理（organize）目标库选择的纯函数：可选目标 = 当前库 + 同类型、有根目录的其他库；
 * 最终重命名路径由后端按所选目标库和目标根预览产出，这里只负责组装提交清单
 * （跨库时附带 `to_library_id`）及判断是否允许打开整理目标选择框。
 *
 * 只允许 `import type`，以便 node --test 直接执行。
 */
import type { MediaLibrary, OrganizePreviewResult } from "./api/libraries";

type OrganizeLibrary = Pick<MediaLibrary, "id" | "kind" | "root_paths">;

export interface OrganizeRenameRequest {
  from: string;
  to: string;
  to_library_id?: string;
}

export function organizeTargets<L extends OrganizeLibrary>(current: L, all: readonly L[]): L[] {
  return all.filter(
    (lib) => lib.id === current.id || (lib.kind === current.kind && lib.root_paths.length > 0),
  );
}

/**
 * 源库全量文件已符合模板时，只要存在其他同类型目标库或本库有多个根目录，
 * 仍允许打开整理对话框选择其他目标库/根目录。
 */
export function canOpenOrganizeDialog(
  library: OrganizeLibrary,
  libraries: readonly OrganizeLibrary[],
  preview: Pick<OrganizePreviewResult, "renames" | "already_ok">,
): boolean {
  if (preview.renames.length > 0) return true;
  if (preview.already_ok === 0) return false;
  const hasCrossLibraryTargets = organizeTargets(library, libraries).some((t) => t.id !== library.id);
  return hasCrossLibraryTargets || library.root_paths.length > 1;
}

export function buildOrganizeRenames(
  renames: readonly { from: string; to: string }[],
  current: Pick<MediaLibrary, "id">,
  target: Pick<MediaLibrary, "id">,
): OrganizeRenameRequest[] {
  if (target.id === current.id) {
    return renames.map(({ from, to }) => ({ from, to }));
  }
  return renames.map(({ from, to }) => ({ from, to, to_library_id: target.id }));
}
